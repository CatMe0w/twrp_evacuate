use chrono::{DateTime, Local};
use flate2::read::DeflateDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use std::{
    collections::HashSet,
    env,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{self, SystemTime},
};
use tar::{Archive, Header};

const DESTINATION_DIR: &str = "twrp_evacuate_migrated";
const DECOMPRESSED_TAR_DIR: &str = "decompressed_temp";
const APK_TEMP_DIR: &str = "apk_temp";

// example of an ApkFsItem: "/data/app/~~YUW09CEoPo_qnb20Rnmw2Q==/com.machiav3lli.backup-DqFd2HhZgfqT9Ep65qCtZQ=="
// root_dir_name: "~~YUW09CEoPo_qnb20Rnmw2Q=="
// instance_dir_name: "com.machiav3lli.backup-DqFd2HhZgfqT9Ep65qCtZQ=="
struct ApkFsItem {
    root_dir_name: String,
    instance_dir_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NeoBackupProperties {
    backup_version_code: i32,
    package_name: String,
    package_label: String,
    version_name: String,
    version_code: i32,
    backup_date: String,
    has_apk: bool,
    has_app_data: bool,
    has_devices_protected_data: bool,
    cpu_arch: String,
    size: i64,
}

struct NeoBackupPropertiesFile {
    name: String,
    content: NeoBackupProperties,
}

type PackageName = String;

type UserId = i32;

fn find_all_win_files(first_win_path: &str) -> Result<Vec<PathBuf>, io::Error> {
    if !first_win_path.ends_with(".win000") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Not a .win000 file",
        ));
    }

    let first_path = Path::new(first_win_path);
    let parent_dir = match first_path.parent() {
        Some(parent) => parent,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Parent directory not found",
            ))
        }
    };

    let file_prefix = first_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.trim_end_matches(".win000"))
        .unwrap_or("");

    let mut win_files: Vec<PathBuf> = fs::read_dir(parent_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|file_name| file_name.starts_with(file_prefix) && file_name.contains(".win"))
                .unwrap_or(false)
        })
        .collect();

    win_files.sort();
    Ok(win_files)
}

fn decompress_win_file(win_path: &PathBuf) -> Result<PathBuf, io::Error> {
    let mut file = File::open(win_path)?;

    // skip gzip header (crc checksum) in case of corrupted files
    let mut header = [0u8; 10];
    file.read_exact(&mut header)?;

    // decompress deflate stream directly
    let mut reader = DeflateDecoder::new(file);
    let mut buffer = Vec::new();
    reader.read_to_end(&mut buffer)?;

    let tar_dir = format!("{}/{}", DESTINATION_DIR, DECOMPRESSED_TAR_DIR);
    fs::create_dir_all(&tar_dir)?;

    let tar_path = format!("{}/{}.tar", tar_dir, win_path.to_string_lossy());
    let mut tar_file = File::create(&tar_path)?;
    tar_file.write_all(&buffer)?;

    Ok(tar_path.into())
}

fn find_all_apks(tar_path: &PathBuf) -> Result<Vec<ApkFsItem>, io::Error> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(file);

    let apk_fs_items: Vec<ApkFsItem> = archive
        .entries()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .path()
                .ok()
                .and_then(|path| path.to_str().map(|s| s.to_string()))
                .filter(|path_str| {
                    path_str.starts_with("/data/app/") && path_str.ends_with("/base.apk")
                })
        })
        .filter_map(|path_str| {
            path_str.split('/').nth(3).and_then(|root_dir_name| {
                path_str
                    .split('/')
                    .nth(4)
                    .map(|instance_dir_name| ApkFsItem {
                        root_dir_name: root_dir_name.to_string(),
                        instance_dir_name: instance_dir_name.to_string(),
                    })
            })
        })
        .collect();

    Ok(apk_fs_items)
}

fn extract_apks_to_temp(tar_path: &PathBuf, apk: &ApkFsItem) -> Result<(), io::Error> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(file);

    let package_name = apk
        .instance_dir_name
        .split('-')
        .next()
        .unwrap_or("")
        .to_string();
    let apk_dir_path = format!("/data/app/{}/{}", apk.root_dir_name, apk.instance_dir_name);
    let dest_dir = format!("{}/{}/{}", DESTINATION_DIR, APK_TEMP_DIR, package_name);

    fs::create_dir_all(&dest_dir)?;

    archive
        .entries()?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path().ok()?;
            let path_str = path.to_str()?;

            if path_str.starts_with(&apk_dir_path) && path_str.ends_with(".apk") {
                let file_name = path.file_name()?.to_string_lossy().to_string();
                Some((entry, file_name))
            } else {
                None
            }
        })
        .try_for_each(|(mut entry, file_name)| {
            let dest_path = format!("{}/{}", dest_dir, file_name);
            let mut dest_file = File::create(dest_path)?;
            io::copy(&mut entry, &mut dest_file)?;
            Ok(())
        })
}

fn find_all_users(tar_path: &PathBuf) -> Result<Vec<i32>, io::Error> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(file);

    let mut user_ids: Vec<i32> = archive
        .entries()?
        .filter_map(|entry| entry.ok()?.path().ok()?.to_str().map(String::from))
        .filter(|path_str| path_str.starts_with("/data/user/"))
        .filter_map(|path_str| path_str.split('/').nth(3)?.parse::<i32>().ok())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    user_ids.sort();
    Ok(user_ids)
}

fn find_all_app_data(
    tar_path: &PathBuf,
    user_id: UserId,
    is_device_protected_data: bool,
) -> Result<Vec<PackageName>, io::Error> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(file);

    let base_path = match user_id {
        0 => "/data/data/",
        _ => &format!("/data/user/{}/", user_id),
    };
    let base_path = match is_device_protected_data {
        true => &format!("/data/user_de/{}/", user_id),
        false => base_path,
    };

    let path_depth = match user_id {
        0 => 4,
        _ => 5,
    };
    let path_depth = if is_device_protected_data { 5 } else { path_depth };

    let mut package_names: Vec<String> = archive
        .entries()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .path()
                .ok()
                .and_then(|path| path.to_str().map(|s| s.to_string()))
        })
        .filter(|path| path.starts_with(base_path))
        .filter_map(|path| {
            path.split('/')
                .nth(path_depth - 1)
                .map(|part| part.to_string())
        })
        .filter(|package_name| !package_name.is_empty())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    package_names.sort();
    Ok(package_names)
}

fn is_tar_empty(tar_path: &PathBuf) -> Result<bool, io::Error> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(file);

    Ok(archive
        .entries()?
        .filter_map(|entry| entry.ok())
        .all(|entry| entry.header().entry_type() != tar::EntryType::Regular))
}

fn extract_app_data(
    tar_path: &PathBuf,
    user_id: UserId,
    package_name: &PackageName,
    is_de_data: bool,
) -> Result<(), io::Error> {
    let file = File::open(tar_path)?;
    let mut archive = Archive::new(file);

    let base_path = match user_id {
        0 => "/data/data",
        _ => &format!("/data/user/{}", user_id),
    };
    let base_path = match is_de_data {
        true => &format!("/data/user_de/{}", user_id),
        false => base_path,
    };

    let data_path = format!("{}/{}", base_path, package_name);
    let dest_dir = format!("{}/{}/{}", DESTINATION_DIR, user_id, package_name);

    let dest_tar_path = match is_de_data {
        true => format!("{}/device_protected_files.tar", dest_dir),
        false => format!("{}/data.tar", dest_dir),
    };
    let dest_gz_path = format!("{}.gz", dest_tar_path);

    fs::create_dir_all(&dest_dir)?;
    let dest_tar_file = File::create(&dest_tar_path)?;
    let mut dest_tar = tar::Builder::new(dest_tar_file);

    archive
        .entries()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            !entry
                .header()
                .groupname()
                .ok()
                .flatten()
                .map(|u| u.ends_with("_cache"))
                .unwrap_or(false)
        })
        .filter_map(|entry| {
            let path = entry.path().ok()?.to_path_buf();
            if path.to_str()?.starts_with(&data_path) && path.strip_prefix(&data_path).is_ok() {
                Some((entry, path))
            } else {
                None
            }
        })
        .filter_map(|(entry, path)| {
            let relative_path = path.strip_prefix(&data_path).ok()?;
            let new_path = Path::new(".").join(relative_path);

            let mut header = Header::new_gnu();
            header.set_size(entry.header().size().ok()?);
            header.set_entry_type(entry.header().entry_type());
            header.set_mode(entry.header().mode().ok()?);
            header.set_uid(entry.header().uid().ok()?);
            header.set_gid(entry.header().gid().ok()?);
            if let Ok(Some(username)) = entry.header().username() {
                header.set_username(username).ok()?;
            }
            if let Ok(Some(groupname)) = entry.header().groupname() {
                header.set_groupname(groupname).ok()?;
            }
            header.set_mtime(entry.header().mtime().ok()?);

            Some((header, new_path, entry))
        })
        .try_for_each(|(mut header, new_path, mut entry)| {
            dest_tar.append_data(&mut header, new_path, &mut entry)
        })?;

    dest_tar.finish()?;

    if is_tar_empty(&dest_tar_path.clone().into())? {
        fs::remove_file(dest_tar_path)?;
    } else {
        let tar_file = File::open(&dest_tar_path)?;
        let gz_file = File::create(&dest_gz_path)?;

        let mut encoder = GzEncoder::new(gz_file, Compression::default());
        io::copy(&mut tar_file.take(u64::MAX), &mut encoder)?;
        encoder.finish()?;

        fs::remove_file(dest_tar_path)?;
    }

    Ok(())
}

fn find_all_extracted_apps(user_id: UserId) -> Result<Vec<PackageName>, io::Error> {
    let all_app_dir = format!("{}/{}", DESTINATION_DIR, user_id);

    let mut extracted_apps = Vec::new();
    for entry in fs::read_dir(&all_app_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let package_name = entry.file_name().to_string_lossy().to_string();
            extracted_apps.push(package_name);
        }
    }

    extracted_apps.sort();
    Ok(extracted_apps)
}

fn move_apks_to_destination(user_id: UserId, package_name: &PackageName) -> Result<(), io::Error> {
    let app_dir = format!("{}/{}/{}", DESTINATION_DIR, user_id, package_name);
    let apk_temp_dir = format!("{}/{}/{}", DESTINATION_DIR, APK_TEMP_DIR, package_name);

    if Path::new(&app_dir).exists() && Path::new(&apk_temp_dir).exists() {
        fs::read_dir(apk_temp_dir)?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("apk"))
            .try_for_each(|entry| {
                let dest_path = format!("{}/{}", app_dir, entry.file_name().to_string_lossy());
                fs::rename(entry.path(), dest_path)
            })?;
    }

    Ok(())
}

fn get_backup_time(win_path: &PathBuf) -> Result<SystemTime, io::Error> {
    let file = File::open(win_path)?;
    let last_modified_time = file.metadata()?.modified()?;
    Ok(last_modified_time)
}

fn make_neo_backup_properties(
    user_id: UserId,
    package_name: &PackageName,
    backup_time: SystemTime,
) -> Result<NeoBackupPropertiesFile, io::Error> {
    // https://github.com/NeoApplications/Neo-Backup/blob/main/TROUBLESHOOTING.md#faking-properties-files-if-they-are-missing-or-damaged
    let app_dir = format!("{}/{}/{}", DESTINATION_DIR, user_id, package_name);

    let has_apk = Path::new(&app_dir).join("base.apk").exists();
    let has_app_data = Path::new(&app_dir).join("data.tar.gz").exists();
    let has_devices_protected_data = Path::new(&app_dir)
        .join("device_protected_files.tar.gz")
        .exists();

    let datetime: DateTime<Local> = DateTime::from(backup_time);
    let properties_datetime = datetime.format("%Y-%m-%dT%H:%M:%S%.3f").to_string();

    let properties = NeoBackupProperties {
        backup_version_code: 8003,
        package_name: package_name.clone(),
        package_label: package_name.clone(),
        version_name: "0.0.0".to_string(),
        version_code: 0,
        backup_date: properties_datetime.clone(),
        has_apk,
        has_app_data,
        has_devices_protected_data,
        cpu_arch: "arm64-v8a".to_string(), // TODO: get this from the extracted APK; at this moment i assume you don't use TWRP/NeoBackup on x86 devices/emulators!
        size: 0,
    };

    let filename_datetime = datetime.format("%Y-%m-%d-%H-%M-%S-%3f").to_string();
    let filename = format!("{}-user_{}", filename_datetime, user_id);

    Ok(NeoBackupPropertiesFile {
        name: filename,
        content: properties,
    })
}

fn assemble_neo_backup_file_structure(
    user_id: UserId,
    package_name: &PackageName,
    properties_file: NeoBackupPropertiesFile,
) -> Result<(), io::Error> {
    let app_dir = format!("{}/{}/{}", DESTINATION_DIR, user_id, package_name);
    let filename = properties_file.name;
    let properties = properties_file.content;

    if !properties.has_apk && !properties.has_app_data && !properties.has_devices_protected_data {
        return Ok(());
    }

    let new_dir = format!("{}/{}", &app_dir, filename);
    fs::create_dir_all(&new_dir)?;

    for entry in fs::read_dir(&app_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            let dest_path = format!("{}/{}", &new_dir, entry.file_name().to_string_lossy());
            fs::rename(&path, &dest_path)?;
        }
    }

    let properties_file_path = format!("{}/{}.properties", &app_dir, filename);
    let properties_file = File::create(properties_file_path)?;
    serde_json::to_writer_pretty(properties_file, &properties)?;

    Ok(())
}

fn cleanup_temp_dir() -> std::io::Result<()> {
    let tar_dir = format!("{}/{}", DESTINATION_DIR, DECOMPRESSED_TAR_DIR);
    let apk_temp_dir = format!("{}/{}", DESTINATION_DIR, APK_TEMP_DIR);
    if Path::new(&tar_dir).exists() {
        fs::remove_dir_all(tar_dir)?;
        fs::remove_dir_all(apk_temp_dir)?;
    }
    Ok(())
}

fn main() -> Result<(), io::Error> {
    let cmdline_args: Vec<String> = env::args().collect();
    if cmdline_args.len() < 2 {
        eprintln!("Usage: {} <path to data.ext4.win000 file>", cmdline_args[0]);
        return Ok(());
    }

    let first_win_path = &cmdline_args[1];
    let win_files = find_all_win_files(first_win_path)?;

    let m = MultiProgress::new();
    let bar_decompress = m.add(ProgressBar::new(win_files.len() as u64));
    bar_decompress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} {bar:20.cyan/blue} {pos}/{len} {msg}")
            .unwrap(),
    );
    bar_decompress.enable_steady_tick(time::Duration::from_millis(100));
    bar_decompress.set_message("Decompressing TWRP backup file(s)");

    let tar_files = win_files
        .iter()
        .map(|win_file| {
            let result = decompress_win_file(win_file);
            bar_decompress.inc(1);
            result
        })
        .collect::<Result<Vec<PathBuf>, io::Error>>()?;
    bar_decompress.finish_and_clear();

    let tar_file_count = tar_files.len();
    let bar_twrp_files = m.add(ProgressBar::new(tar_file_count as u64));
    bar_twrp_files.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} {bar:20.cyan/blue} {pos}/{len} {msg}")
            .unwrap(),
    );
    bar_twrp_files.enable_steady_tick(time::Duration::from_millis(100));
    bar_twrp_files.set_message("Finding users");

    let mut user_ids = tar_files
        .iter()
        .map(find_all_users)
        .collect::<Result<Vec<Vec<i32>>, io::Error>>()?
        .concat();
    user_ids.sort();
    user_ids.dedup();

    let backup_time = get_backup_time(&PathBuf::from(first_win_path))?;

    for tar_file in tar_files {
        bar_twrp_files.set_message("Processing TWRP backup file");
        bar_twrp_files.inc(1);

        let apk_fs_items = find_all_apks(&tar_file)?;
        let bar_apk = m.add(ProgressBar::new(apk_fs_items.len() as u64));
        bar_apk.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} {bar:20.cyan/blue} {pos}/{len} {msg}")
                .unwrap(),
        );
        bar_apk.enable_steady_tick(time::Duration::from_millis(100));
        bar_apk.set_message(format!("Found {} APK(s)", apk_fs_items.len()));

        for apk_fs_item in apk_fs_items {
            bar_apk.set_message(format!(
                "Extracting APK: {}",
                apk_fs_item.instance_dir_name.split('-').next().unwrap()
            ));
            extract_apks_to_temp(&tar_file, &apk_fs_item)?;
            bar_apk.inc(1);
        }
        bar_apk.finish_and_clear();

        let bar_users = m.add(ProgressBar::new(user_ids.len() as u64));
        bar_users.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} {bar:20.cyan/blue} {pos}/{len} {msg}")
                .unwrap(),
        );
        bar_users.enable_steady_tick(time::Duration::from_millis(100));

        for user_id in user_ids.clone() {
            bar_users.set_message("Processing user");
            bar_users.inc(1);

            let app_data = find_all_app_data(&tar_file, user_id, false)?;

            let bar_data = m.add(ProgressBar::new(app_data.len() as u64));
            bar_data.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} {bar:20.cyan/blue} {pos}/{len} {msg}")
                    .unwrap(),
            );
            bar_data.enable_steady_tick(time::Duration::from_millis(100));

            for package_name in app_data {
                bar_data.set_message(format!("Extracting app data: {}", package_name));
                extract_app_data(&tar_file, user_id, &package_name, false)?;
                bar_data.inc(1);
            }

            bar_data.finish_and_clear();

            let app_device_protected_data = find_all_app_data(&tar_file, user_id, true)?;

            let bar_device_protected_data = m.add(ProgressBar::new(app_device_protected_data.len() as u64));
            bar_device_protected_data.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} {bar:20.cyan/blue} {pos}/{len} {msg}")
                    .unwrap(),
            );
            bar_device_protected_data.enable_steady_tick(time::Duration::from_millis(100));

            for package_name in app_device_protected_data {
                bar_device_protected_data.set_message(format!(
                    "Extracting app device protected data: {}",
                    package_name
                ));
                extract_app_data(&tar_file, user_id, &package_name, true)?;
                bar_device_protected_data.inc(1);
            }
            bar_device_protected_data.finish_and_clear();
        }
        bar_users.finish_and_clear();
    }
    bar_twrp_files.finish_and_clear();

    for user_id in user_ids {
        let extracted_apps = find_all_extracted_apps(user_id)?;
        for package_name in extracted_apps {
            move_apks_to_destination(user_id, &package_name)?;
            let properties_file = make_neo_backup_properties(user_id, &package_name, backup_time)?;
            assemble_neo_backup_file_structure(user_id, &package_name, properties_file)?;
        }
    }

    cleanup_temp_dir()?;

    println!();
    println!("========================================");
    println!("All done! Have fun!");
    println!();
    println!("Check the {}/0 directory for the migrated backup, copy them to your device and restore them using Neo Backup.", DESTINATION_DIR);
    println!("If you have more than one user (e.g. work profile), you can find the other users' data in the respective directories (e.g. {}/10, {}/11, etc.)", DESTINATION_DIR, DESTINATION_DIR);
    println!();
    println!("WARNING: Do not restore all backups at once! The migrated backups may contain system apps and data that are not compatible with your device. Restore only the apps you need.");

    Ok(())
}
