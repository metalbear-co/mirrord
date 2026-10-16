//! The first step of the Run view: which folders to run, browsed like a file manager.
//!
//! It opens on the projects the user is likely to want (folders run from before, projects found
//! under the home folder) and from there moves through the disk: `→` or a double-click goes into
//! a folder, `←` goes up, `~` and `/` jump to the home folder and the root. Folders are ticked
//! wherever they are found, so one pick can hold folders from all over the disk.

use std::path::{Path, PathBuf};

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::{
    kit::select_list::{Choice, SelectList},
    screens::{
        run::{
            detect::{self, FOLDER_NAME_SOURCE},
            projects,
        },
        targets::suggest::{self, compress_home},
    },
    theme,
};

/// The id of the row that leaves the project list for the home folder.
const BROWSE_HOME: &str = "browse:home";

/// What the step is showing.
#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    /// The projects found on disk and the folders used before.
    Projects,
    /// The subfolders of one folder.
    Dir(PathBuf),
}

pub struct FolderStep {
    pub list: SelectList,
    pub place: Place,
    /// Folders run from before, listed first in [`Place::Projects`].
    recent: Vec<PathBuf>,
    /// Projects found under the home folder, once the background search is done.
    found: Option<Vec<PathBuf>>,
    /// The folders picked so far, listed first in [`Place::Projects`] so each can be unpicked
    /// there even when it was found somewhere else.
    picked: Vec<PathBuf>,
}

impl FolderStep {
    pub fn new(recent: Vec<PathBuf>, list: SelectList) -> Self {
        let mut step = Self {
            list,
            place: Place::Projects,
            recent,
            found: None,
            picked: Vec::new(),
        };
        step.refresh();
        step
    }

    pub fn is_searching(&self) -> bool {
        self.found.is_none()
    }

    /// Keeps the picked folders listed, refreshing the rows only when they change.
    pub fn set_picked(&mut self, picked: Vec<PathBuf>) {
        if picked != self.picked {
            self.picked = picked;
            self.refresh();
        }
    }

    pub fn set_found(&mut self, found: Vec<PathBuf>) {
        self.found = Some(found);
        self.refresh();
    }

    /// Rebuilds the rows for the place shown.
    pub fn refresh(&mut self) {
        let choices = match &self.place {
            Place::Projects => {
                let mut dirs: Vec<(PathBuf, &'static str)> = self
                    .picked
                    .iter()
                    .map(|dir| (dir.clone(), "picked"))
                    .collect();
                for (dir, section) in self
                    .recent
                    .iter()
                    .map(|dir| (dir, "used before"))
                    .chain(self.found.iter().flatten().map(|dir| (dir, "projects")))
                {
                    if !dirs.iter().any(|(known, _)| known == dir) {
                        dirs.push((dir.clone(), section));
                    }
                }
                let mut choices: Vec<Choice> = dirs
                    .into_iter()
                    .map(|(dir, section)| {
                        let recent = self.recent.contains(&dir);
                        folder_choice(
                            &dir,
                            compress_home(&dir.display().to_string()),
                            recent,
                            Some(section),
                        )
                    })
                    .collect();
                choices.push(Choice {
                    id: BROWSE_HOME.to_owned(),
                    section: Some("browse"),
                    mark: Some(Span::styled("~/", Style::default().fg(theme::INDIGO_LIGHT))),
                    title: "your home folder".to_owned(),
                    detail: "or type ~ or / any time".to_owned(),
                    tone: Some(theme::INDIGO_LIGHT),
                    ..Choice::default()
                });
                choices
            }
            Place::Dir(dir) => {
                let mut choices = vec![Choice {
                    section: Some("this folder"),
                    ..folder_choice(dir, ".".to_owned(), false, None)
                }];
                let mut children: Vec<PathBuf> = std::fs::read_dir(dir)
                    .map(|entries| {
                        entries
                            .flatten()
                            .filter(|entry| entry.path().is_dir())
                            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                            .map(|entry| entry.path())
                            .collect()
                    })
                    .unwrap_or_default();
                children.sort();
                choices.extend(children.iter().map(|child| {
                    let name = child
                        .file_name()
                        .map(|name| format!("{}/", name.to_string_lossy()))
                        .unwrap_or_default();
                    folder_choice(child, name, false, Some("folders"))
                }));
                choices
            }
        };
        self.list.set_choices(choices);
    }

    /// Shows `dir`, with the cursor on `focus` when given.
    pub fn go_to(&mut self, dir: PathBuf, focus: Option<&Path>) {
        self.place = Place::Dir(dir);
        self.list.clear_search();
        self.refresh();
        self.list.focus(&focus.map(id_of).unwrap_or_default());
    }

    /// Goes into the row known as `id`: a folder, or the home folder.
    pub fn open(&mut self, id: &str) {
        if id == BROWSE_HOME {
            if let Some(home) = home() {
                self.go_to(home, None);
            }
            return;
        }
        let path = PathBuf::from(id);
        let is_here = matches!(&self.place, Place::Dir(dir) if *dir == path);
        if path.is_dir() && !is_here {
            self.go_to(path, None);
        }
    }

    /// Goes up a folder, landing on the one just left. Returns whether there was somewhere to go.
    pub fn up(&mut self) -> bool {
        let Place::Dir(dir) = &self.place else {
            return false;
        };
        let dir = dir.clone();
        match dir.parent() {
            Some(parent) => {
                self.go_to(parent.to_path_buf(), Some(&dir));
                true
            }
            None => false,
        }
    }

    pub fn back_to_projects(&mut self) {
        self.place = Place::Projects;
        self.list.clear_search();
        self.refresh();
    }

    /// Where the step is, for the line above the list.
    pub fn location(&self) -> Line<'static> {
        match &self.place {
            Place::Projects => Line::from_iter([
                Span::styled("projects", Style::default().fg(theme::INDIGO_LIGHT)),
                Span::styled(
                    if self.is_searching() {
                        "  still looking in your home folder..."
                    } else {
                        "  found on this computer and used before"
                    },
                    theme::muted(),
                ),
            ]),
            Place::Dir(dir) => Line::from_iter([
                Span::styled("in ", theme::muted()),
                Span::styled(compress_home(&dir.display().to_string()), theme::title()),
            ]),
        }
    }

    /// What the details pane says about the folder under the cursor.
    pub fn details(&self) -> Vec<Line<'static>> {
        let Some(focused) = self.list.focused() else {
            return Vec::new();
        };
        if focused.id == BROWSE_HOME {
            return vec![Line::styled(
                "Look through your home folder for something the search did not find.",
                theme::muted(),
            )];
        }
        let dir = PathBuf::from(&focused.id);
        let names = detect::project_names(&dir);
        let commands = suggest::commands_in(Some(&focused.id));
        let mut lines = vec![
            Line::styled(
                names
                    .first()
                    .map(|name| name.name.clone())
                    .unwrap_or_default(),
                theme::title(),
            ),
            Line::styled(compress_home(&focused.id), theme::muted()),
            Line::raw(""),
        ];
        match names.iter().find(|name| name.source != FOLDER_NAME_SOURCE) {
            Some(name) => lines.push(Line::from_iter([
                Span::styled("project  ", theme::muted()),
                Span::styled(name.source, Style::default().fg(theme::INDIGO_LIGHT)),
            ])),
            None => lines.push(Line::styled(
                "no project files here",
                theme::muted().italic(),
            )),
        }
        match commands.first() {
            Some(command) => lines.push(Line::from_iter([
                Span::styled("runs     ", theme::muted()),
                Span::styled(format!("$ {command}"), Style::default().fg(theme::LAVENDER)),
            ])),
            None => lines.push(Line::styled(
                "no command found - you can type one later",
                theme::muted().italic(),
            )),
        }
        lines
    }
}

/// The id a folder row goes by: its path.
pub fn id_of(dir: &Path) -> String {
    dir.display().to_string()
}

fn folder_choice(dir: &Path, title: String, recent: bool, section: Option<&'static str>) -> Choice {
    let kind = projects::marker(dir);
    let mut note = Vec::new();
    if let Some(kind) = kind {
        note.push(Span::styled(kind, Style::default().fg(theme::INDIGO_LIGHT)));
    }
    if recent {
        if !note.is_empty() {
            note.push(Span::styled(" \u{b7} ", theme::muted()));
        }
        note.push(Span::styled("ran before", Style::default().fg(theme::MINT)));
    }
    Choice {
        id: id_of(dir),
        section,
        title,
        tone: Some(if kind.is_some() {
            theme::LAVENDER
        } else {
            theme::GREY
        }),
        note,
        pickable: true,
        ..Choice::default()
    }
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
