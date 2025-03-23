#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::fs::{ self, File };
use std::io::{ self, Read, Write };
use std::path::{ Path, PathBuf };
use std::sync::{ Arc, Mutex };
use std::thread;

use chrono::Local;
use eframe::egui;
use rfd::FileDialog;
use serde::{ Deserialize, Serialize };
use walkdir::WalkDir;
use zip::{ write::FileOptions, CompressionMethod, ZipArchive, ZipWriter };

use egui::viewport::IconData;

fn load_icon_image() -> Arc<IconData> {
    let image_bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(image_bytes).expect("Invalid image").into_rgba8();
    let (width, height) = image.dimensions();
    let pixels = image.into_raw();

    Arc::new(IconData {
        rgba: pixels,
        width,
        height,
    })
}

fn main() -> Result<(), eframe::Error> {
    let icon = load_icon_image();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder
            ::default()
            .with_inner_size([400.0, 400.0])
            .with_resizable(false)
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "VanManen Backup Tool",
        options,
        Box::new(|_cc| Ok(Box::new(GUIApp::default())))
    )
}

#[derive(Serialize, Deserialize)]
struct BackupTemplate {
    paths: Vec<PathBuf>,
}

struct GUIApp {
    status: Arc<Mutex<String>>,
    selected_folders: Vec<PathBuf>,
}

impl Default for GUIApp {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new("Waiting...".into())),
            selected_folders: Vec::new(),
        }
    }
}

impl eframe::App for GUIApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VanManen Backup Tool");
            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Add Folders").clicked() {
                    if
                        let Some(folders) = FileDialog::new()
                            .set_title("Select folders to back up")
                            .pick_folders()
                    {
                        self.selected_folders.extend(folders);
                        self.selected_folders.sort();
                        self.selected_folders.dedup();
                    }
                }

                if ui.button("Add Files").clicked() {
                    if
                        let Some(files) = FileDialog::new()
                            .set_title("Select files to back up")
                            .pick_files()
                    {
                        self.selected_folders.extend(files);
                        self.selected_folders.sort();
                        self.selected_folders.dedup();
                    }
                }
            });

            ui.add_space(4.0);

            let mut to_remove: Option<usize> = None;
            egui::ScrollArea
                ::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for (i, folder) in self.selected_folders.iter().enumerate() {
                        let label = folder.display().to_string();
                        if ui.button(&label).clicked() {
                            to_remove = Some(i);
                        }
                    }
                });

            if let Some(index) = to_remove {
                self.selected_folders.remove(index);
            }

            ui.separator();

            if ui.button("Save Template").clicked() {
                let save_path = FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .set_title("Save Template")
                    .save_file();

                if let Some(path) = save_path {
                    let template = BackupTemplate {
                        paths: self.selected_folders.clone(),
                    };
                    if let Ok(json) = serde_json::to_string_pretty(&template) {
                        if fs::write(&path, json).is_ok() {
                            *self.status.lock().unwrap() = "✅ Template saved.".into();
                        } else {
                            *self.status.lock().unwrap() = "❌ Failed to save template.".into();
                        }
                    }
                }
            }

            if ui.button("Load Template").clicked() {
                let load_path = FileDialog::new()
                    .add_filter("JSON", &["json"])
                    .set_title("Load Template")
                    .pick_file();

                if let Some(path) = load_path {
                    if let Ok(data) = fs::read_to_string(&path) {
                        if let Ok(template) = serde_json::from_str::<BackupTemplate>(&data) {
                            let flrtd: Vec<PathBuf> = template.paths
                                .into_iter()
                                .filter(|p| p.exists())
                                .collect();
                            self.selected_folders = flrtd;
                            *self.status.lock().unwrap() = "✅ Template loaded.".into();
                        } else {
                            *self.status.lock().unwrap() = "❌ Invalid template format.".into();
                        }
                    }
                }
            }

            if ui.button("Create Backup").clicked() {
                let status = self.status.clone();
                let folders = self.selected_folders.clone();

                if folders.is_empty() {
                    *status.lock().unwrap() = "❌ No folders selected.".into();
                    return;
                }

                thread::spawn(move || {
                    let output_dir = FileDialog::new()
                        .set_title("Select output directory for backup")
                        .pick_folder();

                    if let Some(out) = output_dir {
                        match create_temp_backup_gui(&folders, &out) {
                            Ok(path) => {
                                *status.lock().unwrap() = format!(
                                    "✅ Backup saved to:\n{}",
                                    path.display()
                                );
                            }
                            Err(e) => {
                                *status.lock().unwrap() = format!("❌ Backup failed: {}", e);
                            }
                        }
                    } else {
                        *status.lock().unwrap() = "❌ Output directory not selected.".into();
                    }
                });
            }

            if ui.button("Restore Backup").clicked() {
                let status = self.status.clone();
                thread::spawn(move || {
                    let zip_file = FileDialog::new().add_filter("zip", &["zip"]).pick_file();
                    if let Some(file) = zip_file {
                        match restore_backup_gui(&file) {
                            Ok(_) => {
                                *status.lock().unwrap() = "✅ Restore complete.".into();
                            }
                            Err(e) => {
                                *status.lock().unwrap() = format!("❌ Restore failed: {}", e);
                            }
                        }
                    } else {
                        *status.lock().unwrap() = "❌ No backup selected.".into();
                    }
                });
            }

            ui.separator();
            ui.label(format!("{}", self.status.lock().unwrap()));
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}
fn create_temp_backup_gui(folders: &[PathBuf], output_dir: &PathBuf) -> Result<PathBuf, String> {
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
    let zip_name = format!("backup_{}.zip", timestamp);
    let zip_path = output_dir.join(&zip_name);

    let file = File::create(&zip_path).map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(file);
    let options: FileOptions<'_, ()> = FileOptions::default().compression_method(
        CompressionMethod::Deflated
    );

    zip.start_file("fingerprint.txt", options).unwrap();
    let mut fingerprint = String::from("pillupaa\n[Backup Info]\n");
    for (i, folder) in folders.iter().enumerate() {
        fingerprint.push_str(&format!("Folder {}: {}\n", i + 1, folder.display()));
    }
    zip.write_all(fingerprint.as_bytes()).unwrap();

    for path in folders {
        if path.is_file() {
            let filename = path.file_name().unwrap().to_string_lossy();
            zip.start_file(filename, options).unwrap();
            let mut f = File::open(path).unwrap();
            io::copy(&mut f, &mut zip).unwrap();
        } else if path.is_dir() {
            for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
                let entry_path = entry.path();
                let relative = match entry_path.strip_prefix(path) {
                    Ok(r) => r,
                    Err(_) => {
                        continue;
                    }
                };

                let zip_folder = path.file_name().unwrap();
                let final_path = Path::new(zip_folder).join(relative);

                if entry_path.is_file() {
                    zip.start_file(final_path.to_string_lossy(), options).unwrap();
                    let mut f = File::open(entry_path).unwrap();
                    io::copy(&mut f, &mut zip).unwrap();
                } else if !relative.as_os_str().is_empty() {
                    zip.add_directory(final_path.to_string_lossy(), options).unwrap();
                }
            }
        }
    }

    zip.finish().unwrap();
    Ok(zip_path)
}

fn restore_backup_gui(zip_path: &PathBuf) -> Result<(), String> {
    let file = File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;

    let mut path_map = HashMap::new();
    let mut valid = false;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        if file.name() == "fingerprint.txt" {
            let mut contents = String::new();
            file.read_to_string(&mut contents).unwrap();

            if contents.contains("pillupaa") {
                valid = true;
                for line in contents.lines() {
                    if let Some((_, path)) = line.split_once(": ") {
                        let full_path = PathBuf::from(path);
                        if let Some(name) = full_path.file_name() {
                            path_map.insert(name.to_string_lossy().to_string(), full_path);
                        }
                    }
                }
            }
            break;
        }
    }

    if !valid {
        return Err("Invalid backup fingerprint.".into());
    }

    let current_user_home = dirs::home_dir().unwrap_or(PathBuf::from("C:\\"));

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name_in_zip = file.name();

        if name_in_zip == "fingerprint.txt" {
            continue;
        }

        let zip_path = Path::new(name_in_zip);

        if zip_path.components().count() == 1 {
            if let Some(target) = path_map.get(name_in_zip) {
                let adjusted_path = adjust_path(target, &current_user_home);
                if let Some(parent) = adjusted_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut out = File::create(adjusted_path).map_err(|e| e.to_string())?;
                io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
            }
            continue;
        }

        if let Some(root_component) = zip_path.components().next() {
            let root_name = root_component.as_os_str().to_string_lossy().to_string();
            if let Some(base_original_path) = path_map.get(&root_name) {
                let adjusted_base = adjust_path(base_original_path, &current_user_home);

                let relative_path = zip_path.strip_prefix(&root_name).unwrap();
                let full_path = adjusted_base.join(relative_path);

                if file.name().ends_with('/') {
                    fs::create_dir_all(&full_path).map_err(|e| e.to_string())?;
                } else {
                    if let Some(parent) = full_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    let mut out = File::create(&full_path).map_err(|e| e.to_string())?;
                    io::copy(&mut file, &mut out).map_err(|e| e.to_string())?;
                }
            }
        }
    }

    Ok(())
}

fn adjust_path(original: &PathBuf, current_home: &PathBuf) -> PathBuf {
    let og_str = original.to_string_lossy();
    let current_str = current_home.to_string_lossy();

    if og_str.to_lowercase().starts_with("c:\\users\\") {
        let parts: Vec<&str> = og_str.split('\\').collect();
        if parts.len() > 2 {
            let old_username = parts[2];
            let expected_prefix = format!("C:\\Users\\{}", old_username);

            if og_str.starts_with(&expected_prefix) {
                let rel_path = og_str.strip_prefix(&expected_prefix).unwrap_or("");
                return PathBuf::from(format!("{}{}", current_str, rel_path));
            }
        }
    }

    original.clone()
}
