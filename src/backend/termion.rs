//! Backend using the pure-rust termion library.
//!
//! Requires the `termion-backend` feature.
#![cfg(feature = "termion")]

extern crate termion;

use self::termion::color as tcolor;
use self::termion::event::Event as TEvent;
use self::termion::event::Key as TKey;
use self::termion::event::MouseButton as TMouseButton;
use self::termion::event::MouseEvent as TMouseEvent;
use self::termion::input::{MouseTerminal, TermRead};
use self::termion::raw::{IntoRawMode, RawTerminal};
use self::termion::screen::AlternateScreen;
use self::termion::style as tstyle;
use crossbeam_channel::{self, Receiver, Sender};
use libc;

#[cfg(unix)]
use signal_hook::iterator::Signals;

use backend;
use event::{Event, Key, MouseButton, MouseEvent};
use theme;
use vec::Vec2;

use std::cell::Cell;
use std::io::{Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

/// Backend using termion
pub struct Backend {
    terminal: AlternateScreen<MouseTerminal<RawTerminal<Stdout>>>,
    current_style: Cell<theme::ColorPair>,
}

struct InputParser {
    // Inner state required to parse input
    last_button: Option<MouseButton>,

    event_due: bool,
    requests: Sender<()>,
    input: Receiver<TEvent>,
}

impl InputParser {
    // Creates a non-blocking abstraction over the usual blocking input
    fn new() -> Self {
        let (input_sender, input_receiver) = crossbeam_channel::bounded(0);
        let (request_sender, request_receiver) = crossbeam_channel::bounded(0);

        // This thread will stop after an event when `InputParser` is dropped.
        thread::spawn(move || {
            let stdin = ::std::io::stdin();
            let stdin = stdin.lock();
            let mut events = stdin.events();

            for _ in request_receiver {
                let event: Result<TEvent, ::std::io::Error> =
                    events.next().unwrap();

                if input_sender.send(event.unwrap()).is_err() {
                    return;
                }
            }
        });

        InputParser {
            last_button: None,
            input: input_receiver,
            requests: request_sender,
            event_due: false,
        }
    }

    /// We pledge to want input.
    ///
    /// If we were already expecting input, this is a NO-OP.
    fn request(&mut self) {
        if !self.event_due {
            self.requests.send(()).unwrap();
            self.event_due = true;
        }
    }

    fn peek(&mut self) -> Option<Event> {
        self.request();

        let timeout = ::std::time::Duration::from_millis(10);

        let input = select! {
            recv(self.input) -> input => {
                input
            }
            recv(crossbeam_channel::after(timeout)) -> _ => return None,
        };

        // We got what we came for.
        self.event_due = false;
        Some(self.map_key(input.unwrap()))
    }

    fn next_event(&mut self) -> Event {
        self.request();

        let input = self.input.recv().unwrap();
        self.event_due = false;
        self.map_key(input)
    }

    fn map_key(&mut self, event: TEvent) -> Event {
        match event {
            TEvent::Unsupported(bytes) => Event::Unknown(bytes),
            TEvent::Key(TKey::Esc) => Event::Key(Key::Esc),
            TEvent::Key(TKey::Backspace) => Event::Key(Key::Backspace),
            TEvent::Key(TKey::Left) => Event::Key(Key::Left),
            TEvent::Key(TKey::Right) => Event::Key(Key::Right),
            TEvent::Key(TKey::Up) => Event::Key(Key::Up),
            TEvent::Key(TKey::Down) => Event::Key(Key::Down),
            TEvent::Key(TKey::Home) => Event::Key(Key::Home),
            TEvent::Key(TKey::End) => Event::Key(Key::End),
            TEvent::Key(TKey::PageUp) => Event::Key(Key::PageUp),
            TEvent::Key(TKey::PageDown) => Event::Key(Key::PageDown),
            TEvent::Key(TKey::Delete) => Event::Key(Key::Del),
            TEvent::Key(TKey::Insert) => Event::Key(Key::Ins),
            TEvent::Key(TKey::F(i)) if i < 12 => Event::Key(Key::from_f(i)),
            TEvent::Key(TKey::F(j)) => Event::Unknown(vec![j]),
            TEvent::Key(TKey::Char('\n')) => Event::Key(Key::Enter),
            TEvent::Key(TKey::Char('\t')) => Event::Key(Key::Tab),
            TEvent::Key(TKey::Char(c)) => Event::Char(c),
            TEvent::Key(TKey::Ctrl('c')) => Event::Exit,
            TEvent::Key(TKey::Ctrl(c)) => Event::CtrlChar(c),
            TEvent::Key(TKey::Alt(c)) => Event::AltChar(c),
            TEvent::Mouse(TMouseEvent::Press(btn, x, y)) => {
                let position = (x - 1, y - 1).into();

                let event = match btn {
                    TMouseButton::Left => MouseEvent::Press(MouseButton::Left),
                    TMouseButton::Middle => {
                        MouseEvent::Press(MouseButton::Middle)
                    }
                    TMouseButton::Right => {
                        MouseEvent::Press(MouseButton::Right)
                    }
                    TMouseButton::WheelUp => MouseEvent::WheelUp,
                    TMouseButton::WheelDown => MouseEvent::WheelDown,
                };

                if let MouseEvent::Press(btn) = event {
                    self.last_button = Some(btn);
                }

                Event::Mouse {
                    event,
                    position,
                    offset: Vec2::zero(),
                }
            }
            TEvent::Mouse(TMouseEvent::Release(x, y))
                if self.last_button.is_some() =>
            {
                let event = MouseEvent::Release(self.last_button.unwrap());
                let position = (x - 1, y - 1).into();
                Event::Mouse {
                    event,
                    position,
                    offset: Vec2::zero(),
                }
            }
            TEvent::Mouse(TMouseEvent::Hold(x, y))
                if self.last_button.is_some() =>
            {
                let event = MouseEvent::Hold(self.last_button.unwrap());
                let position = (x - 1, y - 1).into();
                Event::Mouse {
                    event,
                    position,
                    offset: Vec2::zero(),
                }
            }
            _ => Event::Unknown(vec![]),
        }
    }
}

trait Effectable {
    fn on(&self);
    fn off(&self);
}

impl Effectable for theme::Effect {
    fn on(&self) {
        match *self {
            theme::Effect::Simple => (),
            theme::Effect::Reverse => print!("{}", tstyle::Invert),
            theme::Effect::Bold => print!("{}", tstyle::Bold),
            theme::Effect::Italic => print!("{}", tstyle::Italic),
            theme::Effect::Underline => print!("{}", tstyle::Underline),
        }
    }

    fn off(&self) {
        match *self {
            theme::Effect::Simple => (),
            theme::Effect::Reverse => print!("{}", tstyle::NoInvert),
            theme::Effect::Bold => print!("{}", tstyle::NoBold),
            theme::Effect::Italic => print!("{}", tstyle::NoItalic),
            theme::Effect::Underline => print!("{}", tstyle::NoUnderline),
        }
    }
}

impl Backend {
    /// Creates a new termion-based backend.
    pub fn init() -> Box<backend::Backend> {
        print!("{}", termion::cursor::Hide);

        // TODO: lock stdout
        let terminal = AlternateScreen::from(MouseTerminal::from(
            ::std::io::stdout().into_raw_mode().unwrap(),
        ));

        let c = Backend {
            terminal: terminal,
            current_style: Cell::new(theme::ColorPair::from_256colors(0, 0)),
        };

        Box::new(c)
    }

    fn apply_colors(&self, colors: theme::ColorPair) {
        with_color(&colors.front, |c| print!("{}", tcolor::Fg(c)));
        with_color(&colors.back, |c| print!("{}", tcolor::Bg(c)));
    }
}

impl backend::Backend for Backend {
    fn finish(&mut self) {
        print!("{}{}", termion::cursor::Show, termion::cursor::Goto(1, 1));
        print!(
            "{}[49m{}[39m{}",
            27 as char,
            27 as char,
            termion::clear::All
        );
    }

    fn set_color(&self, color: theme::ColorPair) -> theme::ColorPair {
        let current_style = self.current_style.get();

        if current_style != color {
            self.apply_colors(color);
            self.current_style.set(color);
        }

        return current_style;
    }

    fn set_effect(&self, effect: theme::Effect) {
        effect.on();
    }

    fn unset_effect(&self, effect: theme::Effect) {
        effect.off();
    }

    fn has_colors(&self) -> bool {
        // TODO: color support detection?
        true
    }

    fn screen_size(&self) -> Vec2 {
        let (x, y) = termion::terminal_size().unwrap_or((1, 1));
        (x, y).into()
    }

    fn clear(&self, color: theme::Color) {
        self.apply_colors(theme::ColorPair {
            front: color,
            back: color,
        });
        print!("{}", termion::clear::All);
    }

    fn refresh(&mut self) {
        self.terminal.flush().unwrap();
    }

    fn print_at(&self, pos: Vec2, text: &str) {
        print!(
            "{}{}",
            termion::cursor::Goto(1 + pos.x as u16, 1 + pos.y as u16),
            text
        );
    }

    fn start_input_thread(
        &mut self, event_sink: Sender<Option<Event>>,
        input_request: Receiver<backend::InputRequest>,
    ) {
        let running = Arc::new(AtomicBool::new(true));

        #[cfg(unix)]
        {
            backend::resize::start_resize_thread(
                Signals::new(&[libc::SIGWINCH]).unwrap(),
                event_sink.clone(),
                input_request.clone(),
                Arc::clone(&running),
                None,
            );
        }

        let mut parser = InputParser::new();
        thread::spawn(move || {
            for req in input_request {
                match req {
                    backend::InputRequest::Peek => {
                        event_sink.send(parser.peek()).unwrap();
                    }
                    backend::InputRequest::Block => {
                        event_sink.send(Some(parser.next_event())).unwrap();
                    }
                }
            }
            running.store(false, Ordering::Relaxed);
        });
    }
}

fn with_color<F, R>(clr: &theme::Color, f: F) -> R
where
    F: FnOnce(&tcolor::Color) -> R,
{
    match *clr {
        theme::Color::TerminalDefault => f(&tcolor::Reset),
        theme::Color::Dark(theme::BaseColor::Black) => f(&tcolor::Black),
        theme::Color::Dark(theme::BaseColor::Red) => f(&tcolor::Red),
        theme::Color::Dark(theme::BaseColor::Green) => f(&tcolor::Green),
        theme::Color::Dark(theme::BaseColor::Yellow) => f(&tcolor::Yellow),
        theme::Color::Dark(theme::BaseColor::Blue) => f(&tcolor::Blue),
        theme::Color::Dark(theme::BaseColor::Magenta) => f(&tcolor::Magenta),
        theme::Color::Dark(theme::BaseColor::Cyan) => f(&tcolor::Cyan),
        theme::Color::Dark(theme::BaseColor::White) => f(&tcolor::White),

        theme::Color::Light(theme::BaseColor::Black) => f(&tcolor::LightBlack),
        theme::Color::Light(theme::BaseColor::Red) => f(&tcolor::LightRed),
        theme::Color::Light(theme::BaseColor::Green) => f(&tcolor::LightGreen),
        theme::Color::Light(theme::BaseColor::Yellow) => {
            f(&tcolor::LightYellow)
        }
        theme::Color::Light(theme::BaseColor::Blue) => f(&tcolor::LightBlue),
        theme::Color::Light(theme::BaseColor::Magenta) => {
            f(&tcolor::LightMagenta)
        }
        theme::Color::Light(theme::BaseColor::Cyan) => f(&tcolor::LightCyan),
        theme::Color::Light(theme::BaseColor::White) => f(&tcolor::LightWhite),

        theme::Color::Rgb(r, g, b) => f(&tcolor::Rgb(r, g, b)),
        theme::Color::RgbLowRes(r, g, b) => {
            f(&tcolor::AnsiValue::rgb(r, g, b))
        }
    }
}
