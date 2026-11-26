use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::OsString,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use super::{BridgeOptions, ReplyTo, RuntimeOptions};
use crate::profile;

const QUEUED_PROFILES: usize = 4;

pub(super) struct Worker {
    pub(super) wake: UnixStream,
    jobs: SyncSender<Job>,
    completed: Receiver<Completion>,
    queued: RefCell<VecDeque<PendingProfile>>,
    busy: Cell<bool>,
}

pub(super) enum ProfileInput {
    Startup,
    Defaults,
    Invalid(&'static str),
    /// A profile file other than the startup profile, read by the worker.
    File(PathBuf),
    Inline {
        name: Option<OsString>,
        path: Option<PathBuf>,
        contents: Vec<u8>,
    },
}

pub(super) enum ProfileOrigin {
    Startup,
    Defaults,
    Inline {
        name: Option<OsString>,
        path: Option<PathBuf>,
    },
}

pub(super) struct PreparedProfile {
    pub(super) origin: ProfileOrigin,
    pub(super) options: Box<RuntimeOptions>,
}

struct PendingProfile {
    reply: Option<ReplyTo>,
    input: ProfileInput,
}

enum Job {
    Prepare(PendingProfile),
    Remember {
        reply: Option<ReplyTo>,
        selection: PathBuf,
        selected: Option<PathBuf>,
        defaults: bool,
    },
}

pub(super) enum Completion {
    Prepared(Option<ReplyTo>, Result<PreparedProfile, PrepareError>),
    Remembered(Option<ReplyTo>),
}

#[derive(Debug)]
pub(super) enum PrepareError {
    Unavailable,
    Rejected(String),
}

impl Worker {
    pub(super) fn new(startup: Option<PathBuf>, bridge: BridgeOptions) -> io::Result<Self> {
        let (wake, mut notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        let (jobs, work) = mpsc::sync_channel(1);
        let (done, completed) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("jsr-profile".into())
            .spawn(move || {
                while let Ok(job) = work.recv() {
                    let completion = match job {
                        Job::Prepare(PendingProfile { reply, input }) => {
                            Completion::Prepared(reply, prepare(input, startup.as_deref(), bridge))
                        }
                        Job::Remember {
                            reply,
                            selection,
                            selected,
                            defaults,
                        } => {
                            if let Err(error) = profile::remember_selection(
                                &selection,
                                selected.as_deref(),
                                defaults,
                            ) {
                                eprintln!(
                                    "Could not remember active profile {}: {error}",
                                    selection.display()
                                );
                            }
                            Completion::Remembered(reply)
                        }
                    };
                    if done.send(completion).is_err() || notify.write_all(&[1]).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            wake,
            jobs,
            completed,
            queued: RefCell::new(VecDeque::with_capacity(QUEUED_PROFILES)),
            busy: Cell::new(false),
        })
    }

    pub(super) fn try_queue(&self, input: ProfileInput, reply: &mut Option<ReplyTo>) -> bool {
        let mut queued = self.queued.borrow_mut();
        if queued.len() == QUEUED_PROFILES {
            return false;
        }
        queued.push_back(PendingProfile {
            reply: reply.take(),
            input,
        });
        true
    }

    pub(super) fn start_next(&self) -> io::Result<()> {
        if !self.busy.get()
            && let Some(job) = self.queued.borrow_mut().pop_front()
        {
            self.submit(Job::Prepare(job))?;
            self.busy.set(true);
        }
        Ok(())
    }

    pub(super) fn finish(&self) {
        self.busy.set(false);
    }

    pub(super) fn remember(
        &self,
        reply: Option<ReplyTo>,
        selection: PathBuf,
        selected: Option<PathBuf>,
        defaults: bool,
    ) -> io::Result<()> {
        self.submit(Job::Remember {
            reply,
            selection,
            selected,
            defaults,
        })
    }

    fn submit(&self, job: Job) -> io::Result<()> {
        self.jobs
            .try_send(job)
            .map_err(|err| io::Error::other(format!("profile worker: {err}")))
    }

    pub(super) fn receive(&self) -> io::Result<Completion> {
        (&self.wake).read_exact(&mut [0])?;
        self.completed
            .try_recv()
            .map_err(|err| io::Error::other(format!("profile worker: {err}")))
    }
}

pub(super) fn prepare(
    input: ProfileInput,
    startup: Option<&Path>,
    bridge: BridgeOptions,
) -> Result<PreparedProfile, PrepareError> {
    let (origin, profile) = match input {
        ProfileInput::Defaults => {
            return Ok(PreparedProfile {
                origin: ProfileOrigin::Defaults,
                options: Box::default(),
            });
        }
        ProfileInput::Invalid(error) => return Err(PrepareError::Rejected(error.to_owned())),
        ProfileInput::Startup => {
            let path = startup.ok_or(PrepareError::Unavailable)?;
            (ProfileOrigin::Startup, profile::load(path))
        }
        ProfileInput::File(path) => {
            let profile = profile::load(&path);
            (
                ProfileOrigin::Inline {
                    name: path.file_name().map(ToOwned::to_owned),
                    path: Some(path),
                },
                profile,
            )
        }
        ProfileInput::Inline {
            name,
            path,
            contents,
        } => (
            ProfileOrigin::Inline { name, path },
            std::str::from_utf8(&contents)
                .map_err(|err| err.into())
                .and_then(profile::parse),
        ),
    };
    let profile = profile.map_err(|err| PrepareError::Rejected(err.to_string()))?;
    let next = profile.bridge.unwrap_or_default();
    if next.rumble.is_some_and(|value| value != bridge.rumble)
        || next
            .virtual_gamepad
            .is_some_and(|value| value != bridge.virtual_gamepad)
    {
        return Err(PrepareError::Rejected(
            "profile changes to rumble or gamepad enablement require restarting JoyShockRust"
                .to_owned(),
        ));
    }
    Ok(PreparedProfile {
        origin,
        options: Box::new(profile),
    })
}
