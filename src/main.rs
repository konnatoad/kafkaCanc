#![windows_subsystem = "windows"]

mod backup;
mod helpers;
mod restore;

use backup::backup_gui;
use helpers::Progress;
use helpers::build_human_tree;
use helpers::collect_paths;
use helpers::fix_skip;
use helpers::load_icon_image;
use helpers::parse_fingerprint;
use helpers::render_tree;
use restore::restore_backup;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use eframe::egui;
use rfd::FileDialog;
use serde::{Deserialize, Serialize};

type RestoreMsg = Result<(FolderTreeNode, PathBuf), String>;

#[derive(Serialize, Deserialize)]
struct BackupTemplate {
    paths: Vec<PathBuf>,
}

#[derive(Default)]
struct FolderTreeNode {
    children: HashMap<String, FolderTreeNode>,
    checked: bool,
    is_file: bool,
}

#[allow(dead_code)]
fn build_tree_from_paths(paths: &[String]) -> FolderTreeNode {
    let mut root = FolderTreeNode::default();
    for path in paths {
        let mut current = &mut root;
        for part in Path::new(path).components() {
            let key = part.as_os_str().to_string_lossy().to_string();
            current = current
                .children
                .entry(key.clone())
                .or_insert(FolderTreeNode {
                    children: HashMap::new(),
                    checked: true,
                    is_file: false,
                });
        }
        current.is_file = true;
    }
    root
}

// fn update_folder_check_state(node: &mut FolderTreeNode) -> bool {
//     if node.is_file {
//         return node.checked;
//     }
//     let mut all_checked = true;
//     for child in node.children.values_mut() {
//         let child_checked = update_folder_check_state(child);
//         all_checked &= child_checked;
//     }
//
//     node.checked = all_checked;
//     all_checked
// }

fn main() -> Result<(), eframe::Error> {
    println!("[DEBUG] main: Starting application");

    dotenv::dotenv().ok();
    println!("[DEBUG] .env loaded (if present)");

    let icon = load_icon_image();
    println!("[DEBUG] Icon loaded");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([410.0, 450.0])
            .with_resizable(false)
            .with_icon(icon),
        ..Default::default()
    };
    println!("[DEBUG] NativeOptions configured");

    println!("[DEBUG] Launching GUI with run_native");
    eframe::run_native(
        "Konserve",
        options,
        Box::new(|_cc| {
            println!("[DEBUG] GUIApp::default() instantiated");
            Ok(Box::new(GUIApp::default()))
        }),
    )
}

struct GUIApp {
    status: Arc<Mutex<String>>,
    selected_folders: Vec<PathBuf>,
    template_editor: bool,
    template_paths: Vec<PathBuf>,
    restore_editor: bool,
    restore_zip_path: Option<PathBuf>,
    restore_tree: FolderTreeNode,
    _saved_path_map: Option<HashMap<String, PathBuf>>,
    backup_progress: Option<Progress>,
    restore_progress: Option<Progress>,
    restore_opening: bool,
    restore_rx: Option<mpsc::Receiver<RestoreMsg>>,
}

impl Default for GUIApp {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new("Waiting...".to_string())),
            selected_folders: Vec::new(),
            template_editor: false,
            template_paths: Vec::new(),
            restore_editor: false,
            restore_zip_path: None,
            restore_tree: FolderTreeNode::default(),
            _saved_path_map: None,
            backup_progress: None,
            restore_progress: None,
            restore_opening: false,
            restore_rx: None,
        }
    }
}

impl eframe::App for GUIApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(finished_msg) = self.restore_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
                match finished_msg {
                    Ok((mut tree, zip)) => {
                        // NEW: mark everything checked
                        fn check_all(n: &mut FolderTreeNode) {
                            n.checked = true;
                            for c in n.children.values_mut() {
                                check_all(c);
                            }
                        }
                        check_all(&mut tree);

                        self.restore_tree = tree;
                        self.restore_zip_path = Some(zip);
                        self.restore_editor = true;
                    }
                    Err(e) => {
                        *self.status.lock().unwrap() = format!("Failed: {e}");
                    }
                }
                self.restore_rx = None;
            }

            ui.heading("Konserve");
            ui.separator();

            if self.restore_editor {
                ui.label("Restore Selection");

                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        let mut current_path = vec![];
                        render_tree(ui, &mut current_path, &mut self.restore_tree)
                    });

                ui.separator();

                if ui.button("Restore selected").clicked() {
                    if let Some(zip_path) = &self.restore_zip_path.clone() {
                        let selected = collect_paths(&self.restore_tree);
                        let zip_path = zip_path.clone();
                        let status = self.status.clone();

                        let progress = Progress::default();
                        self.restore_progress = Some(progress.clone());
                        self.restore_opening = false;

                        thread::spawn(move || {
                            if let Err(e) =
                                restore_backup(&zip_path, Some(selected), status.clone(), &progress)
                            {
                                *status.lock().unwrap() = format!("❌ Restore failed: {}", e);
                            }
                        });

                        self.restore_editor = false;
                    }
                }

                if ui.button("Cancel").clicked() {
                    self.restore_editor = false;
                    self.restore_zip_path = None;
                    self.restore_tree = FolderTreeNode::default();
                }

                return;
            }

            if self.template_editor {
                ui.label("Editing Template");

                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .max_height(285.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let mut to_remove = None;

                        for (i, path) in self.template_paths.iter_mut().enumerate() {
                            let mut path_str = path.display().to_string();

                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [240.0, 20.0],
                                    egui::TextEdit::singleline(&mut path_str),
                                );

                                if path_str != path.display().to_string() {
                                    *path = PathBuf::from(path_str.clone());
                                }

                                if path.exists() {
                                    ui.label("✅").on_hover_text("This path exists");
                                } else {
                                    ui.label("❌").on_hover_text("This path does not exist");
                                }

                                if ui.button("Browse").clicked() {
                                    if let Some(p) = FileDialog::new().pick_folder() {
                                        *path = p;
                                    }
                                }

                                if ui.button("Remove").clicked() {
                                    to_remove = Some(i);
                                }
                            });
                        }
                        if let Some(i) = to_remove {
                            self.template_paths.remove(i);
                        }
                    });
                ui.separator();
                if ui.button("Add Path").clicked() {
                    self.template_paths.push(PathBuf::new());
                }
                if ui.button("Save Template").clicked() {
                    if let Some(path) = FileDialog::new().add_filter("JSON", &["json"]).save_file()
                    {
                        let tpl = BackupTemplate {
                            paths: self.template_paths.clone(),
                        };
                        match serde_json::to_string_pretty(&tpl) {
                            Ok(json) => {
                                if fs::write(&path, json).is_ok() {
                                    *self.status.lock().unwrap() = "✅ Template saved".into();
                                    self.template_editor = false;
                                } else {
                                    *self.status.lock().unwrap() = "❌ Couldn't write file.".into();
                                }
                            }
                            Err(_) => {
                                *self.status.lock().unwrap() = "❌ Failed to serialize.".into();
                            }
                        }
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.template_editor = false;
                }
                ui.separator();
                ui.label("File names and extensions have to be manually typed in.");

                return;
            }

            ui.horizontal(|ui| {
                if ui.button("Add Folders").clicked() {
                    if let Some(folders) = FileDialog::new().pick_folders() {
                        self.selected_folders.extend(folders);
                        self.selected_folders.sort();
                        self.selected_folders.dedup();
                    }
                }

                if ui.button("Add Files").clicked() {
                    if let Some(files) = FileDialog::new().pick_files() {
                        self.selected_folders.extend(files);
                        self.selected_folders.sort();
                        self.selected_folders.dedup();
                    }
                }
            });

            if !self.selected_folders.is_empty() {
                ui.add_space(4.0);

                // selected paths
                let mut to_remove = None;
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (i, path) in self.selected_folders.iter().enumerate() {
                            if ui.button(path.display().to_string()).clicked() {
                                to_remove = Some(i);
                            }
                        }
                    });
                if let Some(i) = to_remove {
                    self.selected_folders.remove(i);
                }

                ui.add_space(4.0);

                if ui.button("Clear All").clicked() {
                    self.selected_folders.clear();
                }
            }

            ui.separator();

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let btn_size = egui::vec2(95.0, 17.0);
                    //template
                    ui.add_sized(btn_size, egui::Button::new("Load Template"))
                        .clicked()
                        .then(|| {
                            if let Some(path) =
                                FileDialog::new().add_filter("JSON", &["json"]).pick_file()
                            {
                                if let Ok(data) = fs::read_to_string(&path) {
                                    if let Ok(template) =
                                        serde_json::from_str::<BackupTemplate>(&data)
                                    {
                                        let mut valid = Vec::new();
                                        let mut skipped = Vec::new();

                                        for p in template.paths {
                                            match fix_skip(&p) {
                                                Some(adjusted) => valid.push(adjusted),
                                                None => skipped.push(p),
                                            }
                                        }

                                        self.selected_folders = valid;

                                        let msg = if skipped.is_empty() {
                                            "✅ Template loaded".into()
                                        } else {
                                            format!(
                                                "✅ Loaded with {} paths skipped",
                                                skipped.len()
                                            )
                                        };

                                        *self.status.lock().unwrap() = msg;
                                    } else {
                                        *self.status.lock().unwrap() =
                                            "❌ Bad template format.".into();
                                    }
                                }
                            }
                        });

                    ui.add_sized(btn_size, egui::Button::new("Save Template"))
                        .clicked()
                        .then(|| {
                            if let Some(path) =
                                FileDialog::new().add_filter("JSON", &["json"]).save_file()
                            {
                                let template = BackupTemplate {
                                    paths: self.selected_folders.clone(),
                                };

                                if let Ok(json) = serde_json::to_string_pretty(&template) {
                                    if fs::write(&path, json).is_ok() {
                                        *self.status.lock().unwrap() = "✅ Template saved.".into();
                                    } else {
                                        *self.status.lock().unwrap() =
                                            "❌ Failed to write template.".into();
                                    }
                                }
                            }
                        });

                    ui.add_sized(btn_size, egui::Button::new("Edit Template"))
                        .clicked()
                        .then(|| {
                            if let Some(path) =
                                FileDialog::new().add_filter("JSON", &["json"]).pick_file()
                            {
                                if let Ok(data) = fs::read_to_string(&path) {
                                    if let Ok(template) =
                                        serde_json::from_str::<BackupTemplate>(&data)
                                    {
                                        self.template_paths = template
                                            .paths
                                            .into_iter()
                                            .map(|p| fix_skip(&p).unwrap_or(p))
                                            .collect();
                                        self.template_editor = true;
                                    } else {
                                        *self.status.lock().unwrap() =
                                            "❌ Couldn't parse template.".into();
                                    }
                                }
                            }
                        });
                });

                ui.vertical(|ui| {
                    let btn_size = egui::vec2(95.0, 17.0);
                    //backup
                    ui.add_sized(btn_size, egui::Button::new("Create Backup"))
                        .clicked()
                        .then(|| {
                            let folders = self.selected_folders.clone();
                            let status = self.status.clone();

                            if folders.is_empty() {
                                *status.lock().unwrap() = "❌ Nothing selected.".into();
                                return;
                            }

                            *status.lock().unwrap() = "Packing into .tar".into();

                            let progress = Progress::default();
                            self.backup_progress = Some(progress.clone());

                            thread::spawn(move || {
                                if let Some(out_dir) = FileDialog::new()
                                    .set_title("Choose backup destination")
                                    .pick_folder()
                                {
                                    match backup_gui(&folders, &out_dir, &progress) {
                                        Ok(path) => {
                                            *status.lock().unwrap() =
                                                format!("✅ Backup created:\n{}", path.display());
                                        }
                                        Err(e) => {
                                            *status.lock().unwrap() =
                                                format!("❌ Backup failed: {}", e);
                                        }
                                    }
                                } else {
                                    *status.lock().unwrap() = "❌ Cancelled.".into();
                                }
                            });
                        });

                    ui.add_sized(btn_size, egui::Button::new("Restore Backup"))
                        .clicked()
                        .then(|| {
                            let status = self.status.clone();

                            if let Some(zip_file) =
                                FileDialog::new().add_filter("tar", &["tar"]).pick_file()
                            {
                                // show spinner right away
                                self.restore_opening = true;
                                *status.lock().unwrap() = "Opening archive…".into();

                                // prepare a one-shot channel
                                // create a channel of the *new* type
                                let (tx, rx) = mpsc::channel::<RestoreMsg>();
                                self.restore_rx = Some(rx);

                                thread::spawn(move || {
                                    let result: RestoreMsg =
                                        parse_fingerprint(&zip_file).map(|(entries, map)| {
                                            (build_human_tree(entries, map), zip_file.clone())
                                        });
                                    let _ = tx.send(result);
                                });
                            }
                        });
                });
            });

            if self.restore_opening {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(16.0)); // 16 px is default
                    ui.label("Opening archive…");
                });
                ctx.request_repaint_after(std::time::Duration::from_millis(30));
            }

            for opt in [&mut self.backup_progress, &mut self.restore_progress]
                .into_iter()
                .enumerate()
            {
                let (i, p_opt) = opt;
                if let Some(p) = p_opt {
                    let pct = p.get(); // 0‥101   (101 == done)
                    match p.get() {
                        0..=100 => {
                            ui.add(
                                egui::ProgressBar::new((p.get() as f32) / 100.0)
                                    .fill(egui::Color32::from_rgb(80, 160, 240))
                                    .desired_height(6.0)
                                    .animate(true)
                                    .desired_width(ui.available_width()),
                            );
                            ui.add_space(1.0);
                            ui.label(format!("{pct}%"));
                            ui.add_space(1.0);
                            let progress_status = if i == 0 {
                                "Backing up..."
                            } else {
                                "Restoring..."
                            };
                            ui.label(progress_status);
                            ctx.request_repaint_after(std::time::Duration::from_millis(4));
                        }
                        _ => {
                            *p_opt = None;
                        }
                    }
                }
            }
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}
