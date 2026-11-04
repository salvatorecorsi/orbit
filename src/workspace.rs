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
        let tabs: Vec<Tab> = document["tabs"]
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
        let scene_changed = self
            .projects
            .iter()
            .map(|project| (&project.id, &project.title))
            .ne(projects.iter().map(|project| (&project.id, &project.title)))
            || self
                .tabs
                .iter()
                .map(|tab| (&tab.id, &tab.project_id, &tab.title, &tab.kind))
                .ne(tabs
                    .iter()
                    .map(|tab| (&tab.id, &tab.project_id, &tab.title, &tab.kind)));
        self.tabs = tabs;
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
        if scene_changed || self.revision == 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creating_empty_projects_invalidates_the_scene() {
        let mut workspace = OrbitWorkspace::default();
        workspace.apply(json!({"projects": [], "tabs": []}));
        let initial_revision = workspace.revision;

        workspace.apply(json!({
            "projects": [{"id": "first", "title": "Primo progetto"}],
            "tabs": []
        }));
        assert_eq!(workspace.revision, initial_revision + 1);
        assert_eq!(workspace.projects.len(), 1);
        assert!(workspace.tabs.is_empty());

        workspace.apply(json!({
            "projects": [
                {"id": "first", "title": "Primo progetto"},
                {"id": "second", "title": "Secondo progetto"}
            ],
            "tabs": []
        }));
        assert_eq!(workspace.revision, initial_revision + 2);
        assert_eq!(workspace.projects.len(), 2);
        assert!(workspace.tabs.is_empty());
    }

    #[test]
    fn unchanged_scene_and_navigation_updates_preserve_the_revision() {
        let mut document = json!({
            "projects": [{"id": "project", "title": "Progetto"}],
            "tabs": [{
                "id": "tab", "projectId": "project", "title": "Pagina",
                "kind": "browser", "url": "https://example.com"
            }]
        });
        let mut workspace = OrbitWorkspace::default();
        workspace.apply(document.clone());
        let revision = workspace.revision;

        workspace.apply(document.clone());
        assert_eq!(workspace.revision, revision);

        document["tabs"][0]["url"] = json!("https://example.com/next");
        document["scene"] = json!({"currentProjectId": "project"});
        workspace.apply(document);
        assert_eq!(workspace.revision, revision);
        assert_eq!(workspace.tabs[0].url, "https://example.com/next");
    }

    #[test]
    fn scene_labels_and_tab_project_changes_invalidate_the_scene() {
        let mut document = json!({
            "projects": [
                {"id": "first", "title": "Primo progetto"},
                {"id": "second", "title": "Secondo progetto"}
            ],
            "tabs": [{
                "id": "tab", "projectId": "first", "title": "Pagina", "kind": "browser"
            }]
        });
        let mut workspace = OrbitWorkspace::default();
        workspace.apply(document.clone());
        let revision = workspace.revision;

        document["projects"][0]["title"] = json!("Progetto rinominato");
        workspace.apply(document.clone());
        assert_eq!(workspace.revision, revision + 1);

        document["tabs"][0]["title"] = json!("Pagina rinominata");
        workspace.apply(document.clone());
        assert_eq!(workspace.revision, revision + 2);

        document["tabs"][0]["projectId"] = json!("second");
        workspace.apply(document.clone());
        assert_eq!(workspace.revision, revision + 3);

        document["tabs"][0]["kind"] = json!("figma");
        workspace.apply(document);
        assert_eq!(workspace.revision, revision + 4);
    }
}
