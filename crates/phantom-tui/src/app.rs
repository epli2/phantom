use phantom_core::mysql::MysqlTrace;
use phantom_core::trace::HttpTrace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    TraceList,
    TraceDetail,
}

/// Which top-level tab is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveTab {
    #[default]
    Http,
    Mysql,
}

pub struct App {
    // ── HTTP tab ──────────────────────────────────────────────────────────────
    pub traces: Vec<HttpTrace>,
    pub selected_index: usize,
    pub trace_count: u64,

    // ── MySQL tab ─────────────────────────────────────────────────────────────
    pub mysql_traces: Vec<MysqlTrace>,
    pub mysql_selected_index: usize,
    pub mysql_trace_count: u64,

    // ── Shared UI state ───────────────────────────────────────────────────────
    pub filter: String,
    pub filter_active: bool,
    pub active_pane: Pane,
    pub active_tab: ActiveTab,
    pub should_quit: bool,
    pub backend_name: String,
}

impl App {
    pub fn new(backend_name: &str) -> Self {
        Self {
            traces: Vec::new(),
            selected_index: 0,
            trace_count: 0,
            mysql_traces: Vec::new(),
            mysql_selected_index: 0,
            mysql_trace_count: 0,
            filter: String::new(),
            filter_active: false,
            active_pane: Pane::TraceList,
            active_tab: ActiveTab::Http,
            should_quit: false,
            backend_name: backend_name.to_string(),
        }
    }

    // ── HTTP traces ───────────────────────────────────────────────────────────

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
        self.filtered_traces().get(self.selected_index).copied()
    }

    pub fn add_trace(&mut self, trace: HttpTrace) {
        self.traces.insert(0, trace);
        self.trace_count += 1;
        // Keep selection stable when new traces arrive at the top.
        if !self.filter_active && self.selected_index > 0 {
            self.selected_index += 1;
        }
    }

    // ── MySQL traces ──────────────────────────────────────────────────────────

    pub fn filtered_mysql_traces(&self) -> Vec<&MysqlTrace> {
        if self.filter.is_empty() {
            self.mysql_traces.iter().collect()
        } else {
            let filter_lower = self.filter.to_lowercase();
            self.mysql_traces
                .iter()
                .filter(|t| t.query.to_lowercase().contains(&filter_lower))
                .collect()
        }
    }

    pub fn selected_mysql_trace(&self) -> Option<&MysqlTrace> {
        self.filtered_mysql_traces()
            .get(self.mysql_selected_index)
            .copied()
    }

    pub fn add_mysql_trace(&mut self, trace: MysqlTrace) {
        self.mysql_traces.insert(0, trace);
        self.mysql_trace_count += 1;
        if !self.filter_active && self.mysql_selected_index > 0 {
            self.mysql_selected_index += 1;
        }
    }

    // ── Tab switching ─────────────────────────────────────────────────────────

    pub fn switch_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
        self.active_pane = Pane::TraceList;
        self.clear_filter();
    }

    // ── Navigation (tab-aware) ────────────────────────────────────────────────

    pub fn move_up(&mut self) {
        match self.active_tab {
            ActiveTab::Http => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                }
            }
            ActiveTab::Mysql => {
                if self.mysql_selected_index > 0 {
                    self.mysql_selected_index -= 1;
                }
            }
        }
    }

    pub fn move_down(&mut self) {
        match self.active_tab {
            ActiveTab::Http => {
                let max = self.filtered_traces().len().saturating_sub(1);
                if self.selected_index < max {
                    self.selected_index += 1;
                }
            }
            ActiveTab::Mysql => {
                let max = self.filtered_mysql_traces().len().saturating_sub(1);
                if self.mysql_selected_index < max {
                    self.mysql_selected_index += 1;
                }
            }
        }
    }

    pub fn jump_top(&mut self) {
        match self.active_tab {
            ActiveTab::Http => self.selected_index = 0,
            ActiveTab::Mysql => self.mysql_selected_index = 0,
        }
    }

    pub fn jump_bottom(&mut self) {
        match self.active_tab {
            ActiveTab::Http => {
                self.selected_index = self.filtered_traces().len().saturating_sub(1);
            }
            ActiveTab::Mysql => {
                self.mysql_selected_index = self.filtered_mysql_traces().len().saturating_sub(1);
            }
        }
    }

    // ── Pane / filter helpers ─────────────────────────────────────────────────

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
        self.mysql_selected_index = 0;
    }

    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected_index = 0;
        self.mysql_selected_index = 0;
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.selected_index = 0;
        self.mysql_selected_index = 0;
    }
}
