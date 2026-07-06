//! Segment audit (timer.dc.html §Segment audit): the flagged-segments list
//! under Progress — implausibly long, zero/near-zero, missing metadata — with
//! `a` looks-right (acknowledge), `t` trim (a segment-edit preset that
//! shortens the duration to the user's long fence), and `d` delete (asks
//! twice). Flags are derived server-side on read; a clean log means an empty
//! screen and no badge anywhere.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;

use crate::api::{ApiClient, AuditSegment, SegmentUpdate};
use crate::app::action::Action;
use crate::ui::notify::Level;
use crate::ui::{layout::bordered, theme, widgets};

/// Display groups, in rendering order. A row lands in its most severe group
/// (duration shape before metadata); its remaining flags still show inline.
fn group_of(segment: &AuditSegment) -> usize {
    if segment.flags.iter().any(|f| f == "too_long") {
        0
    } else if segment.flags.iter().any(|f| f == "near_zero") {
        1
    } else {
        2
    }
}

const GROUP_TITLES: [&str; 3] = ["IMPLAUSIBLY LONG", "ZERO / NEAR-ZERO", "MISSING METADATA"];

#[derive(Default)]
pub struct Audit {
    audit_count: u32,
    /// Flagged rows, sorted by display group (server order within a group).
    rows: Vec<AuditSegment>,
    selected: usize,
    loading: bool,
    /// A `d` on this row id armed the delete confirm; only the very next `d`
    /// on the same row goes through.
    delete_armed: Option<i64>,
    /// The long fence (hours) from settings — the trim preset's target.
    long_fence_hours: u32,
}

impl Audit {
    pub fn on_enter(&mut self, api: &ApiClient, tx: &UnboundedSender<Action>) {
        self.loading = true;
        spawn_load(api, tx);
        spawn_settings(api, tx);
    }

    fn selected_row(&self) -> Option<&AuditSegment> {
        self.rows.get(self.selected)
    }

    pub async fn handle(
        &mut self,
        action: Action,
        api: &ApiClient,
        tx: &UnboundedSender<Action>,
    ) -> Option<(Level, String)> {
        // The delete confirm is strictly two consecutive `d`s on one row.
        if self.delete_armed.is_some() && !matches!(action, Action::AuditDelete) {
            self.delete_armed = None;
        }
        match action {
            Action::AuditLoaded(read) => {
                self.loading = false;
                self.audit_count = read.audit_count;
                let mut rows = read.segments;
                rows.sort_by_key(group_of);
                self.rows = rows;
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
            }
            Action::AuditReload => {
                self.loading = true;
                spawn_load(api, tx);
            }
            Action::SettingsLoaded(s) => self.long_fence_hours = s.audit_long_hours,
            Action::AuditMove(delta) => {
                if !self.rows.is_empty() {
                    let next = (self.selected as i32 + delta).clamp(0, self.rows.len() as i32 - 1);
                    self.selected = next as usize;
                }
            }
            Action::AuditAcknowledge => {
                if let Some(row) = self.selected_row() {
                    spawn_acknowledge(api, tx, row.id);
                }
            }
            Action::AuditAcknowledged(ack) => {
                self.audit_count = ack.audit_count;
                if let Some(row) = self.rows.iter_mut().find(|r| r.id == ack.segment_id) {
                    row.flags = ack.flags.clone();
                }
                // Fully clean rows leave the list.
                self.rows.retain(|r| !r.flags.is_empty());
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
                return Some((
                    Level::Success,
                    if ack.flags.is_empty() {
                        "looks right — flags cleared for good".into()
                    } else {
                        "duration flags cleared — metadata still needs a fix".into()
                    },
                ));
            }
            Action::AuditTrim => {
                if let Some(row) = self.selected_row() {
                    if self.long_fence_hours == 0 {
                        return Some((
                            Level::Warning,
                            "trim needs the audit fence — settings still loading".into(),
                        ));
                    }
                    if !row.flags.iter().any(|f| f == "too_long") {
                        return Some((
                            Level::Warning,
                            "trim is for implausibly long segments — this row isn't one".into(),
                        ));
                    }
                    spawn_trim(api, tx, row.activity_id, row.id, self.long_fence_hours * 60);
                }
            }
            Action::AuditDelete => {
                let (row_id, activity_id, duration) = self.selected_row().map(|r| {
                    (
                        r.id,
                        r.activity_id,
                        r.formatted_duration.clone().unwrap_or_default(),
                    )
                })?;
                if self.delete_armed != Some(row_id) {
                    self.delete_armed = Some(row_id);
                    return Some((
                        Level::Warning,
                        format!("delete this {duration} segment? `d` again to confirm"),
                    ));
                }
                self.delete_armed = None;
                spawn_delete(api, tx, activity_id, row_id);
            }
            Action::AuditFix => {
                if self.selected_row().is_some() {
                    // The activity edit lives on the Activities table — a soft
                    // handoff until cross-screen deep-links exist.
                    let title = self
                        .selected_row()
                        .and_then(|r| r.activity_title.clone())
                        .unwrap_or_else(|| "the activity".into());
                    let _ = tx.send(Action::Goto(super::ScreenKind::Activities));
                    return Some((
                        Level::Info,
                        format!("fix routes to Activities — find \"{title}\" and press e"),
                    ));
                }
            }
            _ => {}
        }
        None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let title = if self.audit_count > 0 {
            format!("Segment audit · {} flagged", self.audit_count)
        } else {
            "Segment audit".to_string()
        };
        let block = bordered(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.loading && self.rows.is_empty() {
            frame.render_widget(Paragraph::new("loading…"), inner);
            return;
        }
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "  clean log — no flags, no badge, anywhere",
                        Style::default().fg(theme::SUCCESS),
                    )),
                ]),
                inner,
            );
            return;
        }

        let fence_note = |group: usize| -> String {
            match group {
                0 => format!(
                    "over ~{}h (your long fence) · t trim · a looks right",
                    self.long_fence_hours
                ),
                1 => "under your short fence · d delete".into(),
                _ => "no kind / no anchor · f fix".into(),
            }
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut last_group = usize::MAX;
        for (i, row) in self.rows.iter().enumerate() {
            let group = group_of(row);
            if group != last_group {
                last_group = group;
                let count = self.rows.iter().filter(|r| group_of(r) == group).count();
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} {}", GROUP_TITLES[group], count),
                        Style::default()
                            .fg(match group {
                                0 => theme::DANGER,
                                1 => theme::WARN,
                                _ => theme::ACCENT,
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("   {}", fence_note(group)), theme::muted()),
                ]));
            }
            let style = if i == self.selected {
                theme::selection()
            } else {
                Style::default()
            };
            let flags = row.flags.join(" · ").replace('_', " ");
            lines.push(Line::from(vec![
                Span::styled(
                    if i == self.selected { "▌ " } else { "  " }.to_string(),
                    style,
                ),
                Span::styled(
                    row.activity_title
                        .clone()
                        .unwrap_or_else(|| "Untitled timer".into()),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "   {}  {}",
                        row.formatted_duration.clone().unwrap_or_default(),
                        flags
                    ),
                    if i == self.selected {
                        style
                    } else {
                        theme::muted()
                    },
                ),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    pub fn hints(&self) -> Line<'static> {
        widgets::footer_hints(&[
            ("j/k", "move"),
            ("f", "fix"),
            ("a", "looks right"),
            ("t", "trim"),
            ("d", "delete (confirms)"),
            ("h", "progress"),
        ])
    }
}

fn spawn_load(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.progress_audit().await {
            Ok(read) => {
                let _ = tx.send(Action::AuditLoaded(Box::new(read)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("audit load failed: {e}"),
                });
            }
        }
    });
}

fn spawn_settings(api: &ApiClient, tx: &UnboundedSender<Action>) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        if let Ok(s) = api.timer_settings().await {
            let _ = tx.send(Action::SettingsLoaded(Box::new(s)));
        }
    });
}

fn spawn_acknowledge(api: &ApiClient, tx: &UnboundedSender<Action>, segment_id: i64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.acknowledge_audit_segment(segment_id).await {
            Ok(ack) => {
                let _ = tx.send(Action::AuditAcknowledged(Box::new(ack)));
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("acknowledge failed: {e}"),
                });
            }
        }
    });
}

/// The trim preset: one PATCH that shortens the duration down to the long
/// fence — the value that makes the row plausible again.
fn spawn_trim(
    api: &ApiClient,
    tx: &UnboundedSender<Action>,
    activity_id: i64,
    segment_id: i64,
    minutes: u32,
) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api
            .update_segment(
                activity_id,
                segment_id,
                &SegmentUpdate {
                    minutes: Some(minutes),
                },
            )
            .await
        {
            Ok(_) => {
                let _ = tx.send(Action::AuditReload);
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: format!("trimmed to {}h — the plausible span", minutes / 60),
                });
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("trim failed: {e}"),
                });
            }
        }
    });
}

fn spawn_delete(api: &ApiClient, tx: &UnboundedSender<Action>, activity_id: i64, segment_id: i64) {
    let (api, tx) = (api.clone(), tx.clone());
    tokio::spawn(async move {
        match api.delete_segment(activity_id, segment_id).await {
            Ok(()) => {
                let _ = tx.send(Action::AuditReload);
                let _ = tx.send(Action::Notify {
                    level: Level::Success,
                    text: "segment deleted".into(),
                });
            }
            Err(e) => {
                let _ = tx.send(Action::Notify {
                    level: Level::Error,
                    text: format!("delete failed: {e}"),
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Environment};
    use tokio::sync::mpsc;

    fn setup() -> (Audit, ApiClient, mpsc::UnboundedSender<Action>) {
        let config = Config::for_environment(Environment::Development);
        let api = ApiClient::with_token(config.api_url.clone(), "tok".into());
        let (tx, rx) = mpsc::unbounded_channel();
        Box::leak(Box::new(rx));
        (Audit::default(), api, tx)
    }

    fn read(json: serde_json::Value) -> crate::api::AuditRead {
        serde_json::from_value(json).unwrap()
    }

    fn sample() -> crate::api::AuditRead {
        read(serde_json::json!({
            "audit_count": 3,
            "segments": [
                { "id": 1, "activity_id": 9, "activity_title": "Untitled timer",
                  "duration_minutes": 65, "formatted_duration": "1h05m",
                  "flags": ["missing_kind", "missing_anchor"] },
                { "id": 2, "activity_id": 9, "activity_title": "Read DDIA ch.7",
                  "duration_minutes": 485, "formatted_duration": "8h05m",
                  "flags": ["too_long"] },
                { "id": 3, "activity_id": 12, "activity_title": "Consensus · Raft",
                  "duration_minutes": 0, "formatted_duration": "4s",
                  "flags": ["near_zero"] }
            ]
        }))
    }

    #[tokio::test]
    async fn loaded_rows_sort_into_display_groups() {
        let (mut s, api, tx) = setup();
        s.handle(Action::AuditLoaded(Box::new(sample())), &api, &tx)
            .await;
        assert_eq!(s.audit_count, 3);
        // too_long first, then near_zero, then missing metadata.
        assert_eq!(s.rows[0].id, 2);
        assert_eq!(s.rows[1].id, 3);
        assert_eq!(s.rows[2].id, 1);
    }

    #[tokio::test]
    async fn acknowledge_clears_clean_rows_and_keeps_dirty_ones() {
        let (mut s, api, tx) = setup();
        s.handle(Action::AuditLoaded(Box::new(sample())), &api, &tx)
            .await;

        // A fully-clean acknowledge drops the row and updates the badge.
        let ack: crate::api::AuditAcknowledged = serde_json::from_value(serde_json::json!({
            "acknowledged": true, "segment_id": 2, "flags": [], "audit_count": 2
        }))
        .unwrap();
        let note = s
            .handle(Action::AuditAcknowledged(Box::new(ack)), &api, &tx)
            .await;
        assert!(matches!(note, Some((Level::Success, _))));
        assert_eq!(s.rows.len(), 2);
        assert_eq!(s.audit_count, 2);

        // Metadata flags survive an acknowledge — the row stays.
        let ack: crate::api::AuditAcknowledged = serde_json::from_value(serde_json::json!({
            "acknowledged": true, "segment_id": 1,
            "flags": ["missing_kind"], "audit_count": 2
        }))
        .unwrap();
        s.handle(Action::AuditAcknowledged(Box::new(ack)), &api, &tx)
            .await;
        assert!(s.rows.iter().any(|r| r.id == 1));
    }

    #[tokio::test]
    async fn delete_asks_twice_on_the_same_row() {
        let (mut s, api, tx) = setup();
        s.handle(Action::AuditLoaded(Box::new(sample())), &api, &tx)
            .await;
        let warn = s.handle(Action::AuditDelete, &api, &tx).await;
        assert!(matches!(warn, Some((Level::Warning, _))));
        assert!(s.delete_armed.is_some());
        // Moving disarms; the next d warns again.
        s.handle(Action::AuditMove(1), &api, &tx).await;
        assert!(s.delete_armed.is_none());
        assert!(s.handle(Action::AuditDelete, &api, &tx).await.is_some());
        // The second consecutive d goes through.
        assert!(s.handle(Action::AuditDelete, &api, &tx).await.is_none());
    }

    #[tokio::test]
    async fn trim_guards_the_row_kind_and_the_fence() {
        let (mut s, api, tx) = setup();
        s.handle(Action::AuditLoaded(Box::new(sample())), &api, &tx)
            .await;
        // Settings not loaded yet → refuses.
        let warn = s.handle(Action::AuditTrim, &api, &tx).await;
        assert!(matches!(warn, Some((Level::Warning, _))));

        // With the fence, trimming a too_long row is accepted…
        let knobs: crate::api::TimerSettings = serde_json::from_value(serde_json::json!({
            "timer_mode": "stopwatch", "focus_work_minutes": 50,
            "focus_short_break_minutes": 10, "focus_long_break_minutes": 20,
            "focus_long_break_every": 4, "idle_guard_enabled": true,
            "idle_threshold_minutes": 15, "idle_default_reclaim": "trim",
            "audit_long_hours": 6, "audit_short_seconds": 60,
            "audit_badge_enabled": true, "overrun_ping_enabled": true
        }))
        .unwrap();
        s.handle(Action::SettingsLoaded(Box::new(knobs)), &api, &tx)
            .await;
        assert!(s.handle(Action::AuditTrim, &api, &tx).await.is_none());

        // …but a non-long row refuses.
        s.handle(Action::AuditMove(1), &api, &tx).await;
        assert!(s.handle(Action::AuditTrim, &api, &tx).await.is_some());
    }
}
