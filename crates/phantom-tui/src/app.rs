use phantom_core::trace::HttpTrace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    TraceList,
    TraceDetail,
}

pub struct App {
    pub traces: Vec<HttpTrace>,
    pub selected_index: usize,
    pub filter: String,
    pub filter_active: bool,
    pub active_pane: Pane,
    pub should_quit: bool,
    pub trace_count: u64,
    pub backend_name: String,
}

impl App {
    pub fn new(backend_name: &str) -> Self {
        Self {
            traces: Vec::new(),
            selected_index: 0,
            filter: String::new(),
            filter_active: false,
            active_pane: Pane::TraceList,
            should_quit: false,
            trace_count: 0,
            backend_name: backend_name.to_string(),
        }
    }

    pub fn filtered_traces(&self) -> Vec<&HttpTrace> {
        if self.filter.is_empty() {
            self.traces.iter().collect()
        } else {
            let filter_lower = self.filter.to_lowercase();
            self.traces
                .iter()
                .filter(|t| t.url.to_lowercase().contains(&filter_lower))
                .collect()
        }
    }

    pub fn selected_trace(&self) -> Option<&HttpTrace> {
        let filtered = self.filtered_traces();
        filtered.get(self.selected_index).copied()
    }

    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let max = self.filtered_traces().len().saturating_sub(1);
        if self.selected_index < max {
            self.selected_index += 1;
        }
    }

    pub fn jump_top(&mut self) {
        self.selected_index = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.selected_index = self.filtered_traces().len().saturating_sub(1);
    }

    pub fn toggle_pane(&mut self) {
        self.active_pane = match self.active_pane {
            Pane::TraceList => Pane::TraceDetail,
            Pane::TraceDetail => Pane::TraceList,
        };
    }

    pub fn activate_filter(&mut self) {
        self.filter_active = true;
    }

    pub fn deactivate_filter(&mut self) {
        self.filter_active = false;
    }

    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.filter_active = false;
        self.selected_index = 0;
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected_index = 0;
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.selected_index = 0;
    }

    pub fn add_trace(&mut self, trace: HttpTrace) {
        self.traces.insert(0, trace);
        self.trace_count += 1;
        // Keep selection stable when new traces arrive
        if !self.filter_active && self.selected_index > 0 {
            self.selected_index += 1;
        }
    }
}
