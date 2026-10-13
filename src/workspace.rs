use bevy::prelude::*;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Clone)]
pub struct Project {
    pub id: String,
    pub title: String,
    pub center: Vec3,
    pub color: Color,
}

#[derive(Clone)]
pub struct Tab {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub kind: String,
    pub url: String,
    pub document: Value,
}

#[derive(Resource, Default)]
pub struct OrbitWorkspace {
    pub document: Value,
    pub projects: Vec<Project>,
    pub tabs: Vec<Tab>,
    pub current_project: Option<String>,
    pub selected: Option<String>,
    pub expanded: bool,
    pub resolved: HashMap<String, String>,
    pub resolving: Vec<String>,
    pub revision: u64,
    pub shell_dirty: bool,
    pub status: String,
    pub origin: String,
    pub token: String,
}

impl OrbitWorkspace {
    pub fn apply(&mut self, document: Value) {
        let previous_ids: Vec<_> = self.tabs.iter().map(|tab| tab.id.clone()).collect();
        let mut projects = Vec::new();
        if let Some(records) = document["projects"].as_array() {
            for (index, record) in records.iter().enumerate() {
                let angle = index as f32 * 2.399963;
                let radius = if index == 0 {
                    0.0
                } else {
                    26.0 + index as f32 * 3.0
                };
                let colors = [
                    Color::srgb(0.43, 0.76, 0.89),
                    Color::srgb(0.79, 0.56, 0.93),
                    Color::srgb(0.96, 0.66, 0.40),
                    Color::srgb(0.51, 0.86, 0.65),
                ];
                projects.push(Project {
                    id: record["id"].as_str().unwrap_or_default().into(),
                    title: record["title"].as_str().unwrap_or("Progetto").into(),
                    center: Vec3::new(
                        angle.cos() * radius,
                        (index as f32 * 1.7).sin() * 3.0,
                        angle.sin() * radius,
                    ),
                    color: colors[index % colors.len()],
                });
            }
        }
        self.tabs = document["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|record| Tab {
                id: record["id"].as_str().unwrap_or_default().into(),
                project_id: record["projectId"].as_str().unwrap_or("project").into(),
                title: record["title"].as_str().unwrap_or("Finestra").into(),
                kind: record["kind"].as_str().unwrap_or("browser").into(),
                url: record["url"].as_str().unwrap_or_default().into(),
                document: record.clone(),
            })
            .collect();
        if self.revision == 0
            || self
                .current_project
                .as_ref()
                .is_some_and(|id| !projects.iter().any(|project| &project.id == id))
        {
            self.current_project = projects.first().map(|project| project.id.clone());
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.tabs.iter().any(|tab| &tab.id == id))
        {
            self.selected = None;
            self.expanded = false;
        }
        self.projects = projects;
        self.document = document;
        if previous_ids
            != self
                .tabs
                .iter()
                .map(|tab| tab.id.clone())
                .collect::<Vec<_>>()
            || self.revision == 0
        {
            self.revision += 1;
        }
        self.shell_dirty = true;
    }

    pub fn shell_state(&self) -> Value {
        json!({
            "projects": self.projects.iter().map(|project| json!({"id":project.id,"title":project.title})).collect::<Vec<_>>(),
            "tabs": self.tabs.iter().map(|tab| json!({"id":tab.id,"title":tab.title,"kind":tab.kind,"projectId":tab.project_id,"url":tab.url})).collect::<Vec<_>>(),
            "selected": self.selected,
            "expanded": self.expanded,
            "selectedColor": self.selected.as_ref().and_then(|id| self.tab(id)).and_then(|tab| self.projects.iter().find(|project| project.id == tab.project_id)).map(|project| project.color.to_srgba().to_hex()),
            "currentProject": self.current_project,
            "status": self.status,
        })
    }

    pub fn tab(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }
}

#[derive(Clone)]
pub enum WorkspaceAction {
    Select(String),
    Cycle(i32),
    Expand,
    Collapse,
    Deselect,
    Project(String),
    Galaxy,
    Create(Value),
    CreateProject(String),
    Close(String),
    Navigate(String),
    Back,
    Forward,
}

#[derive(Resource, Default)]
pub struct WorkspaceActions(pub Vec<WorkspaceAction>);
