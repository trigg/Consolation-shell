use calloop::{
    channel::{sync_channel, Channel, SyncSender},
    EventLoop,
};
use calloop_wayland_source::WaylandSource;
use cosmic::{
    app::{Command, Core},
    iced::{
        self,
        keyboard::{self, key},
        Length, Subscription,
    },
    widget::{self, button, column, icon, list::container, row},
    ApplicationExt,
};
use cosmic::{widget::text, Application};
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1, EVT_TOPLEVEL_OPCODE},
};
use std::process::exit;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use wayland_client::{
    event_created_child,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_registry::WlRegistry,
        wl_seat::{self, WlSeat},
    },
    Connection, Dispatch, QueueHandle,
};

// Message sent from toplevel manager thread to gui thread
#[derive(Clone, Debug)]
enum ToplevelSignal {
    AddUpdateToplevel((ZwlrForeignToplevelHandleV1, ToplevelDetails)),
    RemoveToplevel(ZwlrForeignToplevelHandleV1),
    SeatChanged(WlSeat),
    //OutputChanged(WlOutput),  // Not using fullscreeb currently
    Closed,
}

// Details about toplevel handles
#[derive(Default, Debug, Clone)]
struct ToplevelDetails {
    title: Option<String>,
    app_id: Option<String>,
    state: Vec<u8>,
    parent: Option<usize>,
}

// Potential actions the gui thread can send back
enum ToplevelAction {
    Refresh(),
    Exit(),
}

struct UiFlags {
    toplevel_recv: Channel<ToplevelSignal>,
    action_sender: SyncSender<ToplevelAction>,
}

struct StagingData {
    exit: bool,
    hash: HashMap<ZwlrForeignToplevelHandleV1, ToplevelDetails>,
    sender: Arc<Mutex<SyncSender<ToplevelSignal>>>,
}
impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for StagingData {
    fn event(
        state: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: <ZwlrForeignToplevelHandleV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let Some(details) = state.hash.get_mut(proxy) {
            match event {
                zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                    details.title = Some(title);
                }
                zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                    details.app_id = Some(app_id);
                }
                zwlr_foreign_toplevel_handle_v1::Event::OutputEnter { output: _ } => {
                    // TODO. Care about oututs
                }
                zwlr_foreign_toplevel_handle_v1::Event::OutputLeave { output: _ } => {
                    // TODO. Care about outputs
                }
                zwlr_foreign_toplevel_handle_v1::Event::State { state } => {
                    details.state = state.clone();
                }
                zwlr_foreign_toplevel_handle_v1::Event::Done => {
                    state
                        .sender
                        .lock()
                        .unwrap()
                        .try_send(ToplevelSignal::AddUpdateToplevel((
                            proxy.clone(),
                            details.clone(),
                        )))
                        .unwrap();
                }
                zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                    state
                        .sender
                        .lock()
                        .unwrap()
                        .try_send(ToplevelSignal::RemoveToplevel(proxy.clone()))
                        .unwrap();
                }
                zwlr_foreign_toplevel_handle_v1::Event::Parent { parent: _ } => {
                    //details.parent = parent;
                }
                _ => todo!(),
            }
        } else {
            unreachable!();
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for StagingData {
    event_created_child!(StagingData, ZwlrForeignToplevelManagerV1, [
        EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ())
    ]);

    fn event(
        state: &mut Self,
        _proxy: &ZwlrForeignToplevelManagerV1,
        event: <ZwlrForeignToplevelManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                state.hash.insert(
                    toplevel,
                    ToplevelDetails {
                        title: None,
                        app_id: None,
                        state: vec![],
                        parent: None,
                    },
                );
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                println!("Manager handle has gone away. We're done here");
                state.exit = true;
            }
            _ => todo!(),
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for StagingData {
    fn event(
        state: &mut Self,
        proxy: &wl_seat::WlSeat,
        event: <wl_seat::WlSeat as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Capabilities { capabilities: _ } => {
                let _ = state
                    .sender
                    .lock()
                    .unwrap()
                    .try_send(ToplevelSignal::SeatChanged(proxy.clone()));
            }
            wl_seat::Event::Name { name: _ } => {}
            _ => todo!(),
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for StagingData {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        todo!();
    }
}

#[tokio::main]
async fn main() {
    let (toplevel_sender, toplevel_recv) = sync_channel::<ToplevelSignal>(50);
    let (action_sender, action_recv) = sync_channel::<ToplevelAction>(10);

    // Start a thread for wayland-client
    // I know this is stupid but I couldn't find a way to
    // hook into the system in cosmic, and their ext-shell-wrapper
    // is in an unusable state for me currently
    let join = {
        tokio::spawn(async {
            let conn = Connection::connect_to_env().unwrap();
            let _display = conn.display();

            let event_queue = conn.new_event_queue();
            let qh = event_queue.handle();
            let (globals, _queue) = registry_queue_init::<StagingData>(&conn).unwrap();
            let _ = globals
                .bind::<ZwlrForeignToplevelManagerV1, StagingData, ()>(&qh, 3..=3, ())
                .unwrap();

            let _ = globals
                .bind::<wl_seat::WlSeat, StagingData, ()>(&qh, 1..=1, ())
                .unwrap();

            let mut event_loop: EventLoop<StagingData> = EventLoop::try_new().unwrap();
            let loop_handle = event_loop.handle();
            loop_handle
                .insert_source(action_recv, |event, _meta, state| match event {
                    calloop::channel::Event::Msg(msg) => match msg {
                        ToplevelAction::Refresh() => {}
                        ToplevelAction::Exit() => state.exit = true,
                    },
                    calloop::channel::Event::Closed => {
                        println!("Channel close. Ending");
                        state.exit = true;
                    }
                })
                .expect("Unable to register channel");

            WaylandSource::new(conn, event_queue)
                .insert(loop_handle)
                .expect("Unable to register wayland");

            let toplevel_sender2 = toplevel_sender.clone();
            let mut state = StagingData {
                exit: false,
                hash: HashMap::new(),
                sender: Arc::new(Mutex::new(toplevel_sender)),
            };

            while let Ok(_) = event_loop.dispatch(Some(Duration::from_millis(100)), &mut state) {
                if state.exit {
                    break;
                }
            }
            let _ = toplevel_sender2.try_send(ToplevelSignal::Closed);
        })
    };

    let input = UiFlags {
        toplevel_recv: toplevel_recv,
        action_sender,
    };

    let mut settings = cosmic::app::Settings::default();
    settings = settings.transparent(true);
    settings = settings.client_decorations(false);
    cosmic::app::run::<ConsolationSwitcherApp>(settings, input).expect("Unable to start App");
    let _ = join.await;
    exit(0);
}

struct ConsolationSwitcherApp {
    core: Core,
    toplevel_recv: RefCell<Option<Channel<ToplevelSignal>>>,
    action_sender: SyncSender<ToplevelAction>,
    applist: HashMap<ZwlrForeignToplevelHandleV1, ToplevelDetails>,
    seat: Option<WlSeat>,
    //    output: Option<WlOutput>,
    selection: ConsolationSelection,
}

#[derive(Debug)]
enum ConsolationSelection {
    WindowActivate(usize),
    WindowMaxToggle(usize),
    WindowClose(usize),
    RunButton,
}

#[derive(Debug, Clone)]
enum Message {
    // Messages from channel
    UpdateApplication(ZwlrForeignToplevelHandleV1, ToplevelDetails),
    RemoveApplication(ZwlrForeignToplevelHandleV1),
    NewSeat(WlSeat),
    //NewOutput(WlOutput),
    // Messages from user
    ActivateApplication(ZwlrForeignToplevelHandleV1),
    MinApplication(ZwlrForeignToplevelHandleV1),
    MaxApplication(ZwlrForeignToplevelHandleV1),
    UnMaxApplication(ZwlrForeignToplevelHandleV1),
    CloseApplication(ZwlrForeignToplevelHandleV1),
    // Message from user keyboard
    ArrowUp(),
    ArrowDown(),
    ArrowLeft(),
    ArrowRight(),
    Select(),
    Back(),

    NoOp(),
    Finish(),
}

impl Application for ConsolationSwitcherApp {
    type Executor = cosmic::executor::Default;
    type Flags = UiFlags;
    type Message = Message;

    const APP_ID: &'static str = "Consolation Switcher";

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (
            ConsolationSwitcherApp {
                core,
                toplevel_recv: RefCell::new(Some(flags.toplevel_recv)),
                action_sender: flags.action_sender,
                applist: HashMap::new(),
                seat: None,
                selection: ConsolationSelection::WindowActivate(0),
            },
            Command::none(),
        )
    }

    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Message::UpdateApplication(k, v) => {
                self.applist.insert(k, v);
            }
            Message::RemoveApplication(k) => {
                self.applist.remove(&k);
            }
            Message::ActivateApplication(app) => {
                if let Some(seat) = self.seat.clone() {
                    app.activate(&seat);
                }
                let _ = self.action_sender.try_send(ToplevelAction::Refresh());
                return self.minimize();
                //let _ = self.action_sender.try_send(ToplevelAction::Exit());
            }
            Message::MinApplication(app) => {
                app.set_minimized();
                let _ = self.action_sender.try_send(ToplevelAction::Refresh());
            }
            Message::MaxApplication(app) => {
                app.set_maximized();
                let _ = self.action_sender.try_send(ToplevelAction::Refresh());
            }
            Message::UnMaxApplication(app) => {
                app.unset_maximized();
                let _ = self.action_sender.try_send(ToplevelAction::Refresh());
            }
            Message::CloseApplication(app) => {
                app.close();
                // Wake other thread up to process this
                let _ = self.action_sender.try_send(ToplevelAction::Refresh());
            }
            Message::NoOp() => {}
            Message::Finish() => {
                exit(0);
            }
            Message::NewSeat(seat) => {
                self.seat = Some(seat);
            }

            Message::ArrowUp() => match self.selection {
                ConsolationSelection::WindowActivate(idx) => {
                    if idx == 0 {
                        self.selection = ConsolationSelection::RunButton;
                    } else {
                        self.selection = ConsolationSelection::WindowActivate(idx - 1);
                    }
                }
                ConsolationSelection::WindowMaxToggle(idx) => {
                    if idx == 0 {
                        self.selection = ConsolationSelection::RunButton;
                    } else {
                        self.selection = ConsolationSelection::WindowMaxToggle(idx - 1);
                    }
                }
                ConsolationSelection::WindowClose(idx) => {
                    if idx == 0 {
                        self.selection = ConsolationSelection::RunButton;
                    } else {
                        self.selection = ConsolationSelection::WindowClose(idx - 1);
                    }
                }
                _ => {}
            },
            Message::ArrowDown() => match self.selection {
                ConsolationSelection::WindowActivate(idx) => {
                    if idx < self.applist.len() {
                        self.selection = ConsolationSelection::WindowActivate(idx + 1);
                    }
                }
                ConsolationSelection::WindowMaxToggle(idx) => {
                    if idx < self.applist.len() {
                        self.selection = ConsolationSelection::WindowMaxToggle(idx + 1);
                    }
                }
                ConsolationSelection::WindowClose(idx) => {
                    if idx < self.applist.len() {
                        self.selection = ConsolationSelection::WindowClose(idx + 1);
                    }
                }
                ConsolationSelection::RunButton => {
                    self.selection = ConsolationSelection::WindowActivate(0);
                }
            },
            Message::ArrowLeft() => {
                if let ConsolationSelection::WindowMaxToggle(idx) = self.selection {
                    self.selection = ConsolationSelection::WindowActivate(idx);
                }
                if let ConsolationSelection::WindowClose(idx) = self.selection {
                    self.selection = ConsolationSelection::WindowMaxToggle(idx);
                }
            }
            Message::ArrowRight() => {
                if let ConsolationSelection::WindowMaxToggle(idx) = self.selection {
                    self.selection = ConsolationSelection::WindowClose(idx);
                }
                if let ConsolationSelection::WindowActivate(idx) = self.selection {
                    self.selection = ConsolationSelection::WindowMaxToggle(idx);
                }
            }
            Message::Select() => todo!(),
            Message::Back() => todo!(),
        }
        Command::none()
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Self::Message> {
        Subscription::batch([
            iced::subscription::unfold(
                "toplevel changes",
                self.toplevel_recv.take(),
                move |mut recvr| async move {
                    let change = recvr.as_mut().unwrap().recv().unwrap().clone();
                    (
                        match change {
                            ToplevelSignal::AddUpdateToplevel(update) => {
                                Message::UpdateApplication(update.0, update.1)
                            }
                            ToplevelSignal::RemoveToplevel(delete) => {
                                Message::RemoveApplication(delete)
                            }
                            ToplevelSignal::Closed => Message::Finish(),
                            ToplevelSignal::SeatChanged(seat) => Message::NewSeat(seat),
                        },
                        recvr,
                    )
                },
            ),
            keyboard::on_key_press(|key, modifiers| {
                let keyboard::Key::Named(key) = key else {
                    return None;
                };

                match (key, modifiers) {
                    (key::Named::ArrowUp, _) => Some(Message::ArrowUp()),
                    (key::Named::ArrowDown, _) => Some(Message::ArrowDown()),
                    (key::Named::ArrowLeft, _) => Some(Message::ArrowLeft()),
                    (key::Named::ArrowRight, _) => Some(Message::ArrowRight()),
                    (key::Named::Enter, _) => Some(Message::Select()),
                    (key::Named::Escape, _) => Some(Message::Back()),
                    _ => None,
                }
            }),
        ])
    }

    fn on_app_exit(&mut self) -> Option<Self::Message> {
        let _ = self.action_sender.try_send(ToplevelAction::Exit());
        Some(Message::NoOp())
    }

    fn view(&self) -> cosmic::Element<Self::Message> {
        let mut c = column();
        let _row_maybe = match self.selection {
            ConsolationSelection::WindowActivate(idx) => Some(idx),
            ConsolationSelection::WindowMaxToggle(idx) => Some(idx),
            ConsolationSelection::WindowClose(idx) => Some(idx),
            _ => None,
        };
        let is_min = zwlr_foreign_toplevel_handle_v1::State::Maximized as u8;
        let is_act = zwlr_foreign_toplevel_handle_v1::State::Activated as u8;
        println!("{:?}", self.selection);
        for (app, details) in self.applist.iter() {
            if details.title.is_none() || details.title.clone().unwrap() == "nil" {
                continue;
            }

            let highlight = details.state.contains(&is_act);
            let mut row2 = row();
            let mut row = row();

            let icon = icon::from_name(details.app_id.clone().unwrap_or("nil".to_owned()));
            let mut title = details
                .title
                .clone()
                .unwrap_or("No title".to_owned())
                .to_owned();
            if highlight {
                title = "++ ".to_string() + &title;
            }
            let text = text(title);

            row = row.push(icon);
            row = row.push(text);
            let mut activate_button = button(row);
            //if highlight { activate_button = activate_button.style()}
            activate_button = activate_button.on_press(Message::ActivateApplication(app.clone()));
            row2 = row2.push(activate_button);
            row2 = row2.push(widget::Space::with_width(Length::Fill));

            if !details.state.contains(&is_min) {
                let mut max_button = button(icon::from_name("window-maximize"));
                max_button = max_button.on_press(Message::MaxApplication(app.clone()));
                row2 = row2.push(max_button);
            } else {
                let mut max_button = button(icon::from_name("window-restore"));
                max_button = max_button.on_press(Message::UnMaxApplication(app.clone()));
                row2 = row2.push(max_button);
            }

            let mut close_button = button(icon::from_name("window-close"));
            close_button = close_button.on_press(Message::CloseApplication(app.clone()));
            row2 = row2.push(close_button);

            let mut container = container(row2);
            container = container.style(match highlight {
                true => cosmic::theme::Container::Background,
                false => cosmic::theme::Container::Transparent,
            });
            c = c.push(container);
        }
        c.into()
    }
}
