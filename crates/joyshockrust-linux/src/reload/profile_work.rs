use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use super::{
    BridgeOptions, Client, InlineProfile, MAX_PROFILE_BYTES, RuntimeOptions, inline_profile,
};
use crate::profile;

const QUEUED_PROFILES: usize = 4;

pub(super) struct Worker {
    pub(super) wake: UnixStream,
    jobs: SyncSender<Job>,
    completed: Receiver<Completion>,
    queued: RefCell<VecDeque<Option<Client>>>,
    busy: Cell<bool>,
}

enum Job {
    Prepare(Option<Client>),
    Remember {
        client: Option<Client>,
        selection: PathBuf,
        selected: Option<PathBuf>,
        defaults: bool,
    },
}

pub(super) enum Completion {
    Prepared(Option<Client>, Result<Box<RuntimeOptions>, PrepareError>),
    Remembered(Option<Client>),
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
                        Job::Prepare(client) => {
                            let prepared = prepare(client.as_ref(), startup.as_deref(), bridge);
                            Completion::Prepared(client, prepared)
                        }
                        Job::Remember {
                            client,
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
                            Completion::Remembered(client)
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

    pub(super) fn try_queue(&self, client: &mut Option<Client>) -> bool {
        let mut queued = self.queued.borrow_mut();
        if queued.len() == QUEUED_PROFILES {
            return false;
        }
        queued.push_back(client.take());
        true
    }

    pub(super) fn start_next(&self) -> io::Result<()> {
        if !self.busy.get()
            && let Some(client) = self.queued.borrow_mut().pop_front()
        {
            self.submit(Job::Prepare(client))?;
            self.busy.set(true);
        }
        Ok(())
    }

    pub(super) fn finish(&self) {
        self.busy.set(false);
    }

    pub(super) fn remember(
        &self,
        client: Option<Client>,
        selection: PathBuf,
        selected: Option<PathBuf>,
        defaults: bool,
    ) -> io::Result<()> {
        self.submit(Job::Remember {
            client,
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
    client: Option<&Client>,
    startup: Option<&Path>,
    bridge: BridgeOptions,
) -> Result<Box<RuntimeOptions>, PrepareError> {
    if super::is_unload(client) {
        return Ok(Box::default());
    }
    let profile = if let Some(InlineProfile { contents, .. }) =
        inline_profile(client).map_err(|err| PrepareError::Rejected(err.to_owned()))?
    {
        if contents.len() > MAX_PROFILE_BYTES {
            return Err(PrepareError::Rejected("profile exceeds 64 KiB".to_owned()));
        }
        std::str::from_utf8(contents)
            .map_err(|err| err.into())
            .and_then(profile::parse)
    } else if let Some(path) = startup {
        profile::load(path)
    } else {
        return Err(PrepareError::Unavailable);
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
    Ok(Box::new(profile))
}
