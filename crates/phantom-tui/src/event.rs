use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyEvent};

pub enum Event {
    Key(KeyEvent),
    Tick,
}

pub struct EventHandler {
    tick_rate: Duration,
}

impl EventHandler {
    pub fn new(tick_rate_ms: u64) -> Self {
        Self {
            tick_rate: Duration::from_millis(tick_rate_ms),
        }
    }

    /// Poll for the next event. Returns `None` if no event within tick rate.
    pub fn poll(&self) -> std::io::Result<Event> {
        if event::poll(self.tick_rate)?
            && let CrosstermEvent::Key(key) = event::read()?
        {
            return Ok(Event::Key(key));
        }
        Ok(Event::Tick)
    }
}
