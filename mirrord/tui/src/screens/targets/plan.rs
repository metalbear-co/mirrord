//! The session plan pane: the ordered list of services that will make up the
//! emitted `mirrord-up.yaml`, plus the export dialog's draft and registry.

use std::path::PathBuf;

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, List, ListItem, ListState, Paragraph},
};
use strum::VariantArray;
use strum_macros::{Display, VariantArray};

use crate::{
    helpers::{centered, ellipsize},
    screens::targets::{
        form::{SettingDef, WidgetKind, join_command},
        model::{CommonSpec, ServiceEntry, UpFile},
        theme,
    },
};

#[derive(Default)]
pub struct PlanPane {
    pub services: Vec<ServiceEntry>,
    pub selected: usize,
    list_state: ListState,
}

impl PlanPane {
    pub fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_down(&mut self) {
        self.selected = (self.selected + 1).min(self.services.len().saturating_sub(1));
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.services.swap(self.selected, self.selected - 1);
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.services.len() {
            self.services.swap(self.selected, self.selected + 1);
            self.selected += 1;
        }
    }

    pub fn delete_selected(&mut self) {
        if self.selected < self.services.len() {
            self.services.remove(self.selected);
            self.selected = self.selected.min(self.services.len().saturating_sub(1));
        }
    }

    pub fn toggle_skip_selected(&mut self) {
        if let Some(service) = self.services.get_mut(self.selected) {
            service.spec.skip = !service.spec.skip;
        }
    }

    /// Adds a new service or replaces the one being edited, keeping names
    /// unique by suffixing duplicates (`web`, `web-2`, ...).
    pub fn upsert(&mut self, mut entry: ServiceEntry, editing: Option<usize>) {
        let taken = |name: &str, services: &[ServiceEntry]| {
            services
                .iter()
                .enumerate()
                .any(|(index, service)| Some(index) != editing && service.name == name)
        };
        if taken(&entry.name, &self.services) {
            let base = entry.name.clone();
            let mut counter = 2;
            while taken(&format!("{base}-{counter}"), &self.services) {
                counter += 1;
            }
            entry.name = format!("{base}-{counter}");
        }

        match editing {
            Some(index) if index < self.services.len() => {
                self.services[index] = entry;
                self.selected = index;
            }
            _ => {
                self.services.push(entry);
                self.selected = self.services.len() - 1;
            }
        }
    }

    pub fn up_file(&self, common: &CommonSpec) -> UpFile {
        // The Directory field is TUI-side sugar - `mirrord up` has no
        // per-service directory. A dir equal to the TUI's own cwd is
        // where the command runs anyway and just drops out; any other dir
        // is folded into the command as a shell `cd`.
        let cwd = std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let mut services = self.services.clone();
        for service in &mut services {
            let run = &mut service.spec.run;
            match run.dir.take() {
                None => {}
                Some(dir) if dir == cwd || run.command.is_empty() => {}
                Some(dir) => run.command = fold_dir(&dir, &run.command),
            }
        }

        UpFile {
            common: common.clone(),
            services,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let border = if focused {
            theme::BRAND
        } else {
            theme::BORDER_DIM
        };
        let title = if self.services.is_empty() {
            " Session Plan ".to_owned()
        } else {
            format!(" Session Plan · {} ", self.services.len())
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(theme::TEXT_EMPHASIS)
                    .bg(theme::FILL_HEAVY)
                    .bold(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.services.is_empty() {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("No services yet.", Style::default().fg(theme::TEXT_MUTED)),
                    Line::raw(""),
                    Line::styled(
                        "Pick a target on the left (Enter) to add one.",
                        Style::default().fg(theme::TEXT_MUTED).italic(),
                    ),
                ])
                .centered(),
                centered(inner, Constraint::Fill(1), Constraint::Length(3)),
            );
            return;
        }

        self.selected = self.selected.min(self.services.len() - 1);
        let width = inner.width as usize;
        let items: Vec<ListItem> = self
            .services
            .iter()
            .enumerate()
            .map(|(index, service)| {
                // The selected service expands into its full settings; the
                // rest stay compact one-liners.
                if index == self.selected {
                    expanded_item(index, service, width)
                } else {
                    compact_item(index, service, width)
                }
            })
            .collect();

        let list = List::new(items).highlight_style(
            Style::default()
                .bg(theme::FILL_HEAVY)
                .fg(theme::TEXT_EMPHASIS)
                .bold(),
        );
        self.list_state.select(Some(self.selected));
        frame.render_stateful_widget(list, inner, &mut self.list_state);
    }
}

/// One-line summary: name, target, mode. Mode and skip always stay visible;
/// the name and the target path split what is left, and the target truncates
/// first since the name is what the user chose to call it.
fn compact_item(index: usize, service: &ServiceEntry, width: usize) -> ListItem<'static> {
    let number = format!(" {}. ", index + 1);
    let mode = format!("  {}", service.spec.default_mode);
    let skip = if service.spec.skip { "  skip" } else { "" };

    let shared = width
        .saturating_sub(number.chars().count())
        .saturating_sub(mode.chars().count())
        .saturating_sub(skip.chars().count());
    let name = ellipsize(&service.name, shared.saturating_sub(12).max(12));
    let target_budget = shared.saturating_sub(name.chars().count() + 2);
    let target = ellipsize(&service.spec.target.display(), target_budget);

    let mut spans = vec![
        Span::styled(number, Style::default().fg(theme::TEXT_MUTED)),
        Span::styled(name, Style::default().fg(theme::TEXT_EMPHASIS).bold()),
        Span::styled(
            format!("  {target}"),
            Style::default().fg(theme::TEXT_MUTED),
        ),
        // `replace` takes the target's traffic over entirely - worth a
        // louder color than the shared-traffic default.
        Span::styled(
            mode,
            match service.spec.default_mode {
                crate::screens::targets::model::ServiceMode::Replace => {
                    Style::default().fg(theme::WARNING)
                }
                _ => Style::default().fg(theme::FILL_DIM),
            },
        ),
    ];
    if service.spec.skip {
        spans.push(Span::styled(
            skip,
            Style::default().fg(theme::WARNING).italic(),
        ));
    }
    ListItem::new(Line::from(spans))
}

/// The selected service, unfolded: every setting on its own line, values
/// shown in full (only clamped to the pane width) instead of ellipsized.
fn expanded_item(index: usize, service: &ServiceEntry, width: usize) -> ListItem<'static> {
    let spec = &service.spec;
    let detail = |label: &str, value: String| {
        Line::from_iter([
            Span::styled(
                format!("      {label:<9}"),
                Style::default().fg(theme::FILL_DIM),
            ),
            Span::styled(
                ellipsize(&value, width.saturating_sub(15)),
                Style::default().fg(theme::TEXT_MUTED),
            ),
        ])
    };

    let mut header = vec![
        Span::styled(
            format!(" {}. ", index + 1),
            Style::default().fg(theme::TEXT_MUTED),
        ),
        Span::styled(
            ellipsize(&service.name, width.saturating_sub(12)),
            Style::default().fg(theme::TEXT_EMPHASIS).bold(),
        ),
    ];
    if spec.skip {
        header.push(Span::styled(
            "  skip",
            Style::default().fg(theme::WARNING).italic(),
        ));
    }

    let mut lines = vec![
        Line::from(header),
        detail("target", spec.target.display()),
        detail("mode", spec.default_mode.to_string()),
    ];
    if let Some(filter) = spec
        .http_filter
        .as_ref()
        .and_then(|filter| filter.header_filter.as_deref())
    {
        lines.push(detail("filter", filter.to_owned()));
    }
    if !spec.ignore_ports.is_empty() {
        let ports = spec
            .ignore_ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(detail("ignores", ports));
    }
    lines.push(detail(
        "command",
        match spec.run.command.is_empty() {
            true => "(not set - needed to run)".to_owned(),
            false => join_command(&spec.run.command),
        },
    ));

    ListItem::new(lines)
}

/// Output file format. `mirrord up` reads yaml; json is for tooling that
/// prefers it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, VariantArray, Display)]
#[strum(serialize_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Yaml,
    Json,
}

impl ExportFormat {
    pub fn default_path(self) -> &'static str {
        match self {
            Self::Yaml => "./mirrord-up.yaml",
            Self::Json => "./mirrord-up.json",
        }
    }
}

/// Draft edited by the export dialog: where to write, in which format, and
/// the `common` settings that go at the top of the file.
#[derive(Clone)]
pub struct ExportDraft {
    pub path: String,
    pub format: ExportFormat,
    pub common: CommonSpec,
}

impl Default for ExportDraft {
    fn default() -> Self {
        Self {
            path: ExportFormat::Yaml.default_path().to_owned(),
            format: ExportFormat::Yaml,
            common: CommonSpec::default(),
        }
    }
}

/// Renders an `Option<bool>` as an auto/on/off tri-state select.
fn tristate_index(value: Option<bool>) -> usize {
    match value {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    }
}

fn tristate_value(index: usize) -> Option<bool> {
    match index {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// Emulates the per-service directory: the emitted command becomes a
/// shell that changes into it before exec'ing the real command, since
/// `mirrord up` always spawns services from its own cwd.
fn fold_dir(dir: &str, command: &[String]) -> Vec<String> {
    let quoted: Vec<String> = command.iter().map(|part| shell_quote(part)).collect();
    vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!("cd {} && exec {}", shell_quote(dir), quoted.join(" ")),
    ]
}

/// Quotes one word for POSIX `sh`: plain words pass through, anything
/// else gets single quotes (with embedded quotes escaped).
fn shell_quote(part: &str) -> String {
    let plain = !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        part.to_owned()
    } else {
        format!("'{}'", part.replace('\'', r"'\''"))
    }
}

const TRISTATE: &[&str] = &["auto", "on", "off"];

/// The export dialog registry: file settings first, then the `common`
/// settings written at the top of the emitted file.
pub const EXPORT_SETTINGS: &[SettingDef<ExportDraft>] = &[
    SettingDef {
        label: "File",
        help: "where to write the config",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Text {
            get: |draft| draft.path.clone(),
            set: |draft, value| {
                if value.is_empty() {
                    return Err("the file needs a path".to_owned());
                }
                draft.path = value.to_owned();
                Ok(())
            },
        },
    },
    SettingDef {
        label: "Format",
        help: "mirrord up reads yaml; json is for other tooling",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Select {
            options: &["yaml", "json"],
            get: |draft| draft.format as usize,
            set: |draft, index| {
                let previous = draft.format;
                draft.format = ExportFormat::VARIANTS[index % ExportFormat::VARIANTS.len()];
                // Follow the format with the default file name, but never
                // clobber a path the user typed themselves.
                if draft.path == previous.default_path() {
                    draft.path = draft.format.default_path().to_owned();
                }
            },
        },
    },
    SettingDef {
        label: "Operator",
        help: "force the mirrord operator on/off for all services; auto = detect",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Select {
            options: TRISTATE,
            get: |draft| tristate_index(draft.common.operator),
            set: |draft, index| draft.common.operator = tristate_value(index),
        },
    },
    SettingDef {
        label: "Kube context",
        help: "kube context for all services; empty = current context",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Text {
            get: |draft| draft.common.context.clone().unwrap_or_default(),
            set: |draft, value| {
                draft.common.context = (!value.is_empty()).then(|| value.to_owned());
                Ok(())
            },
        },
    },
    SettingDef {
        label: "Insecure TLS",
        help: "accept invalid cluster certificates; auto = mirrord's default",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Select {
            options: TRISTATE,
            get: |draft| tristate_index(draft.common.accept_invalid_certificates),
            set: |draft, index| {
                draft.common.accept_invalid_certificates = tristate_value(index);
            },
        },
    },
];

pub fn validate_export(draft: &ExportDraft) -> Result<(), String> {
    if draft.path.trim().is_empty() {
        return Err("the file needs a path".to_owned());
    }
    Ok(())
}

/// Writes the plan to the drafted path. Returns the written path.
pub fn write_export(draft: &ExportDraft, file: &UpFile) -> anyhow::Result<PathBuf> {
    let rendered = match draft.format {
        ExportFormat::Yaml => file.to_yaml()?,
        ExportFormat::Json => file.to_json()?,
    };
    let path = PathBuf::from(draft.path.trim());
    std::fs::write(&path, rendered)
        .map_err(|error| anyhow::anyhow!("failed to write {}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::targets::model::{
        RunSpec, ServiceEntry, ServiceMode, ServiceSpec, TargetSpec,
    };

    fn plan_with(dir: Option<&str>, command: &[&str]) -> PlanPane {
        PlanPane {
            services: vec![ServiceEntry {
                name: "svc".to_owned(),
                spec: ServiceSpec {
                    target: TargetSpec::Targetless,
                    default_mode: ServiceMode::Split,
                    http_filter: None,
                    ignore_ports: Default::default(),
                    skip: false,
                    run: RunSpec {
                        command: command.iter().map(|part| (*part).to_owned()).collect(),
                        dir: dir.map(str::to_owned),
                        ..Default::default()
                    },
                },
            }],
            ..Default::default()
        }
    }

    /// `mirrord up` has no per-service directory, so the emitted command
    /// carries the `cd` itself.
    #[test]
    fn up_file_folds_the_directory_into_a_shell_cd() {
        let plan = plan_with(Some("/work/my app"), &["npm", "start"]);
        let file = plan.up_file(&Default::default());

        let run = &file.services[0].spec.run;
        assert_eq!(run.dir, None);
        assert_eq!(
            run.command,
            ["sh", "-c", "cd '/work/my app' && exec npm start"],
        );
    }

    /// The TUI's own cwd is where the commands run anyway; no wrapping.
    #[test]
    fn up_file_drops_the_default_directory() {
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let plan = plan_with(Some(&cwd), &["npm", "start"]);
        let file = plan.up_file(&Default::default());

        let run = &file.services[0].spec.run;
        assert_eq!(run.dir, None);
        assert_eq!(run.command, ["npm", "start"]);
    }

    #[test]
    fn shell_quote_passes_plain_words_and_quotes_the_rest() {
        assert_eq!(shell_quote("./zoo-echo"), "./zoo-echo");
        assert_eq!(shell_quote("-listen"), "-listen");
        assert_eq!(shell_quote("hello world"), "'hello world'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
    }
}
