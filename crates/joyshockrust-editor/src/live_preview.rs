//! One demand-driven bridge subscription per editor window.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    path::PathBuf,
    rc::Rc,
};

use adw::{glib, gtk, prelude::*};
use joyshockrust_ipc::{
    stream::Topic,
    telemetry::preview::Preview,
    unix::subscriptions::{Subscription, read_preview},
};

type Listener = Rc<dyn Fn(Option<&Preview>)>;

#[derive(Clone)]
pub struct LivePreview(Rc<Hub>);

impl Default for LivePreview {
    fn default() -> Self {
        Self(Rc::new(Hub {
            path: joyshockrust_config::paths::runtime_socket(joyshockrust_ipc::STATUS_SOCKET, true),
            ..Hub::default()
        }))
    }
}

#[derive(Default)]
struct Hub {
    path: Option<PathBuf>,
    next: Cell<u64>,
    listeners: RefCell<BTreeMap<u64, Listener>>,
    stream: RefCell<Option<Stream>>,
}

struct Stream {
    subscription: Subscription<Preview>,
    wake_source: Option<glib::SourceId>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(source) = self.wake_source.take() {
            source.remove();
        }
    }
}

impl LivePreview {
    pub fn watch(
        &self,
        widget: &impl IsA<gtk::Widget>,
        update: impl Fn(Option<&Preview>) + 'static,
    ) {
        let index = self.0.next.get();
        self.0.next.set(index + 1);
        let update: Listener = Rc::new(update);
        let map = {
            let hub = self.0.clone();
            move || {
                hub.listeners.borrow_mut().insert(index, update.clone());
                let disconnected = hub.stream.borrow().is_none();
                if disconnected && let Ok(stream) = hub.connect() {
                    hub.stream.replace(Some(stream));
                }
                update(None);
                if let Some(preview) = hub.latest() {
                    update(Some(&preview));
                }
            }
        };
        let on_map = map.clone();
        widget.connect_map(move |_| on_map());
        let hub = self.0.clone();
        widget.connect_unmap(move |_| {
            hub.listeners.borrow_mut().remove(&index);
            if hub.listeners.borrow().is_empty() {
                hub.stream.take();
            }
        });
        // Trigger controls can be attached to an already visible page.
        if widget.as_ref().is_mapped() {
            map();
        }
    }
}

impl Hub {
    fn connect(self: &Rc<Self>) -> io::Result<Stream> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "bridge runtime directory"))?;
        let (input, wake) = UnixDatagram::pair()?;
        input.set_nonblocking(true)?;
        wake.set_nonblocking(true)?;
        let subscription = Subscription::start(path, Topic::Preview, read_preview, move || {
            let _ = wake.send(&[1]);
        })?;
        let weak = Rc::downgrade(self);
        let wake_source = glib::source::unix_fd_add_local(
            input.as_raw_fd(),
            glib::IOCondition::IN,
            move |_, _| {
                // At most a few notifications can accumulate before latest() is called.
                for _ in 0..8 {
                    if input.recv(&mut [0]).is_err() {
                        break;
                    }
                }
                if let Some(hub) = weak.upgrade() {
                    hub.dispatch();
                }
                glib::ControlFlow::Continue
            },
        );
        Ok(Stream {
            subscription,
            wake_source: Some(wake_source),
        })
    }

    fn latest(&self) -> Option<Preview> {
        self.stream.borrow().as_ref()?.subscription.latest()
    }

    fn dispatch(&self) {
        let preview = self.latest();
        let listeners: Vec<_> = self.listeners.borrow().values().cloned().collect();
        for update in listeners {
            update(preview.as_ref());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixListener,
        thread,
        time::{Duration, Instant},
    };

    use joyshockrust_ipc::{
        stream::{HEADER_LEN, Stamp},
        telemetry::preview::History,
    };

    use super::*;

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn mapped_widgets_share_one_stream_and_release_it_after_unmap() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status");
        let listener = UnixListener::bind(Topic::Preview.socket_path(&path)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let live = LivePreview(Rc::new(Hub {
            path: Some(path),
            ..Hub::default()
        }));
        let window = gtk::Window::new();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
        window.set_child(Some(&root));
        let widgets = [gtk::Label::new(None), gtk::Label::new(None)];
        let counts = Rc::new([const { Cell::new(0) }; 2]);
        for (index, widget) in widgets.iter().enumerate() {
            let counts = counts.clone();
            live.watch(widget, move |frame| {
                if frame.is_some_and(|frame| frame.history.session == 9) {
                    counts[index].set(counts[index].get() + 1);
                }
            });
            root.append(widget);
        }
        assert!(live.0.stream.borrow().is_none());
        window.present();
        let main = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut client = loop {
            while main.pending() {
                main.iteration(false);
            }
            match listener.accept() {
                Ok((client, _)) => break client,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => panic!("{error}"),
            }
            assert!(Instant::now() < deadline, "editor did not subscribe");
            thread::sleep(Duration::from_millis(5));
        };
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; HEADER_LEN];
        client.read_exact(&mut request).unwrap();
        let id = Topic::Preview.parse_subscription(&request).unwrap();
        let history = History {
            session: 9,
            device: None,
            first: 0,
            samples: vec![],
            imu: None,
        };
        client
            .write_all(
                &history
                    .frame(
                        id,
                        Stamp {
                            instance: 1,
                            revision: 1,
                        },
                    )
                    .unwrap(),
            )
            .unwrap();
        while counts.iter().any(|count| count.get() == 0) {
            assert!(
                Instant::now() < deadline,
                "both listeners should see the shared frame"
            );
            main.iteration(false);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        root.remove(&widgets[0]);
        assert!(live.0.stream.borrow().is_some());
        root.remove(&widgets[1]);
        assert!(live.0.stream.borrow().is_none());
        assert_eq!(client.read(&mut [0]).unwrap(), 0);
        window.close();
    }
}
