//! System tray icon (StatusNotifierItem): works on KDE, on Ubuntu's GNOME (AppIndicator
//! extension) and on other desktops with a tray host.

use crate::report::fmt_dur;

#[derive(Debug, Default, Clone)]
pub struct TrayState {
    /// Project counting right now.
    pub current: Option<String>,
    /// Seconds today per project, largest first.
    pub today: Vec<(String, i64)>,
    /// All-time seconds of the current project.
    pub current_total: i64,
}

impl TrayState {
    pub fn headline(&self) -> String {
        match &self.current {
            Some(p) => {
                let today = self.today.iter().find(|(n, _)| n == p).map(|(_, s)| *s).unwrap_or(0);
                format!("{p} · {} today · {} total", fmt_dur(today), fmt_dur(self.current_total))
            }
            None => {
                let sum: i64 = self.today.iter().map(|(_, s)| s).sum();
                format!("Paused · {} today", fmt_dur(sum))
            }
        }
    }
}

pub struct CodeclockTray {
    pub state: TrayState,
    /// The dashboard's address, when it is running.
    pub dashboard: Option<String>,
}

/// Opens a URL in the default browser.
pub fn open_url(url: &str) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
        log::warn!("could not open {url}: {e}");
    }
}

impl ksni::Tray for CodeclockTray {
    fn id(&self) -> String {
        "codeclock".into()
    }

    /// Left click opens the dashboard.
    fn activate(&mut self, _x: i32, _y: i32) {
        if let Some(url) = &self.dashboard {
            open_url(url);
        }
    }

    fn title(&self) -> String {
        self.state.headline()
    }

    fn icon_name(&self) -> String {
        if self.state.current.is_some() {
            "media-playback-start-symbolic".into()
        } else {
            "media-playback-pause-symbolic".into()
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.state.headline(),
            description: self
                .state
                .today
                .iter()
                .map(|(p, s)| format!("{p}: {}", fmt_dur(*s)))
                .collect::<Vec<_>>()
                .join("\n"),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem { label: self.state.headline(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
        ];
        if self.state.today.is_empty() {
            items.push(StandardItem { label: "No time yet today".into(), enabled: false, ..Default::default() }.into());
        }
        for (p, s) in &self.state.today {
            items.push(StandardItem { label: format!("{p}   {}", fmt_dur(*s)), enabled: false, ..Default::default() }.into());
        }
        items.push(MenuItem::Separator);
        if let Some(url) = self.dashboard.clone() {
            items.push(
                StandardItem {
                    label: "Open history".into(),
                    icon_name: "x-office-calendar".into(),
                    activate: Box::new(move |_| open_url(&url)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}
