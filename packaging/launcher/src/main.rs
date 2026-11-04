#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Cursor, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const PAYLOAD: &[u8] = include_bytes!(env!("ORBIT_PAYLOAD_ZIP"));
const PAYLOAD_HASH: &str = env!("ORBIT_PAYLOAD_SHA256");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    if let Err(error) = run() {
        let directory = data_directory().unwrap_or_else(|_| env::temp_dir().join("Orbit"));
        let log = directory.join("runtime").join("launcher.log");
        let _ = log_message(&log, &format!("ERROR: {error}"));
        show_error(&format!(
            "Impossibile avviare ORBIT.\n\n{error}\n\nDettagli: {}",
            log.display()
        ));
        std::process::exit(1);
    }
}

fn data_directory() -> Result<PathBuf> {
    if let Some(directory) = env::var_os("ORBIT_DATA_DIRECTORY") {
        return Ok(absolute(PathBuf::from(directory))?);
    }
    let local = env::var_os("LOCALAPPDATA")
        .ok_or("La cartella dati Windows LOCALAPPDATA non è disponibile.")?;
    Ok(PathBuf::from(local).join("Orbit"))
}

fn absolute(path: PathBuf) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn run() -> Result<()> {
    let data = data_directory()?;
    let log = data.join("runtime").join("launcher.log");
    log_message(&log, &format!("Starting payload {PAYLOAD_HASH}"))?;
    let cache = match env::var_os("ORBIT_CACHE_DIRECTORY") {
        Some(directory) => absolute(PathBuf::from(directory))?,
        None => PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA non disponibile")?)
            .join("Orbit")
            .join("versions"),
    };
    let version = prepare_payload(&cache)?;
    log_message(&log, &format!("Runtime: {}", version.display()))?;
    let mut arguments: Vec<OsString> = env::args_os().skip(1).collect();
    if arguments.iter().any(|value| value == "--extract-only") {
        println!("{}", version.display());
        return Ok(());
    }
    // Resolve caller-supplied relative paths before switching to the bundle directory.
    if let Some(index) = arguments.iter().position(|value| value == "--projects") {
        let project_directory = arguments
            .get_mut(index + 1)
            .ok_or("Specifica una cartella dopo --projects.")?;
        *project_directory = absolute(PathBuf::from(&*project_directory))?.into_os_string();
    }

    let app = version.join("app");
    ensure_webview(&app.join("runtime"), &log)?;
    let runtime = data.join("runtime");
    let projects = data.join("projects");
    let profile = data.join("profile");
    for directory in [&runtime, &projects, &profile] {
        fs::create_dir_all(directory)?;
    }
    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime.join("orbit.log"))?;
    let errors = OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime.join("orbit-errors.log"))?;
    let mut command = Command::new(app.join("orbit-rust.exe"));
    command
        .args(&arguments)
        .current_dir(&app)
        .env("ORBIT_APP_DIRECTORY", &app)
        .env(
            "ORBIT_NODE_EXECUTABLE",
            app.join("runtime").join("node.exe"),
        )
        .env(
            "ORBIT_CODE_EXECUTABLE",
            app.join("runtime").join("code-tunnel.exe"),
        )
        .env("ORBIT_RUNTIME_DIRECTORY", &runtime)
        .env("ORBIT_RUST_STATE_DIRECTORY", &runtime)
        .env("ORBIT_RUST_PROFILE_DIRECTORY", &profile)
        .env("ORBIT_PROJECTS_DIRECTORY", &projects)
        .env("ORBIT_EMBEDDING_CACHE", data.join("model-cache"))
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors))
        .stdin(Stdio::null());
    if !arguments.iter().any(|value| value == "--projects") {
        command.arg("--projects").arg(&projects);
    }
    // Native add-ons load their redistributable C++ DLLs from this directory.
    let mut search_paths = vec![app.join("runtime"), app.clone()];
    search_paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    command.env("PATH", env::join_paths(search_paths)?);
    let status = command.status()?;
    log_message(&log, &format!("Application exited: {status}"))?;
    if !status.success() {
        return Err(format!(
            "ORBIT si è chiuso con errore ({status}).\nControlla {}",
            runtime.join("orbit-errors.log").display()
        )
        .into());
    }
    Ok(())
}

fn payload_ready(version: &Path) -> bool {
    fs::read_to_string(version.join("complete")).is_ok_and(|contents| contents == PAYLOAD_HASH)
        && [
            "app/orbit-rust.exe",
            "app/runtime/node.exe",
            "app/services/host.js",
        ]
        .iter()
        .all(|filename| version.join(filename).is_file())
}

fn prepare_payload(cache: &Path) -> Result<PathBuf> {
    if PAYLOAD_HASH.len() != 64 || !PAYLOAD_HASH.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err("Identificativo del pacchetto non valido.".into());
    }
    fs::create_dir_all(cache)?;
    let version = cache.join(PAYLOAD_HASH);
    // OS file locks are released even if a launcher is terminated during extraction.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join(format!("{PAYLOAD_HASH}.lock")))?;
    lock.lock()?;
    if payload_ready(&version) {
        return Ok(version);
    }
    let actual_hash = format!("{:x}", Sha256::digest(PAYLOAD));
    if actual_hash != PAYLOAD_HASH {
        return Err("Il pacchetto ORBIT è danneggiato: verifica SHA-256 fallita.".into());
    }
    let staging = cache.join(format!("{PAYLOAD_HASH}.extracting-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir(&staging)?;
    if let Err(error) = extract_archive(PAYLOAD, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::write(staging.join("complete"), PAYLOAD_HASH)?;
    if !payload_ready(&staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err("Il pacchetto ORBIT non contiene tutti i file richiesti.".into());
    }
    if version.exists() {
        // Only this hash-specific cache is replaced; user projects live elsewhere.
        fs::remove_dir_all(&version)?;
    }
    fs::rename(&staging, &version)?;
    Ok(version)
}

fn extract_archive(bytes: &[u8], directory: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let relative = file
            .enclosed_name()
            .ok_or("Percorso non valido nel pacchetto")?;
        if relative.components().any(|component| {
            !matches!(component, Component::Normal(_))
                || component.as_os_str().to_string_lossy().contains(':')
        }) || file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Percorso o collegamento non consentito nel pacchetto".into());
        }
        let destination = directory.join(relative);
        if file.is_dir() {
            fs::create_dir_all(destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::create(destination)?;
            io::copy(&mut file, &mut output)?;
        }
    }
    Ok(())
}

fn log_message(filename: &Path, message: &str) -> io::Result<()> {
    if let Some(parent) = filename.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(filename)?;
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    writeln!(output, "[{seconds}] {message}")
}

#[cfg(windows)]
fn hidden_command(executable: impl AsRef<std::ffi::OsStr>) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new(executable);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(not(windows))]
fn hidden_command(executable: impl AsRef<std::ffi::OsStr>) -> Command {
    Command::new(executable)
}

fn webview_installed() -> bool {
    let system = env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let reg = PathBuf::from(system).join("System32").join("reg.exe");
    let key = r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    [("HKCU", "/reg:64"), ("HKLM", "/reg:32")]
        .iter()
        .any(|(hive, view)| {
            hidden_command(&reg)
                .args(["query", &format!("{hive}\\{key}"), "/v", "pv", view])
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                            let fields: Vec<_> = line.split_whitespace().collect();
                            fields.len() >= 3
                                && fields[0] == "pv"
                                && fields[2]
                                    .split('.')
                                    .any(|part| part.parse::<u32>().unwrap_or(0) > 0)
                        })
                })
        })
}

fn ensure_webview(runtime: &Path, log: &Path) -> Result<()> {
    if webview_installed() {
        return Ok(());
    }
    log_message(log, "Installing Microsoft Edge WebView2 runtime")?;
    let status = hidden_command(runtime.join("MicrosoftEdgeWebview2Setup.exe"))
        .args(["/silent", "/install"])
        .status()?;
    if !webview_installed() {
        return Err(format!(
            "Il runtime Microsoft Edge WebView2 non è disponibile.\nLa preparazione automatica richiede Internet (esito: {status}).\nConnettiti e riapri Orbit.exe."
        ).into());
    }
    Ok(())
}

#[cfg(windows)]
fn show_error(message: &str) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "ORBIT".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), message.as_ptr(), title.as_ptr(), 0x10);
    }
}

#[cfg(not(windows))]
fn show_error(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{ZipWriter, write::SimpleFileOptions};

    #[test]
    fn archives_cannot_escape_the_extraction_directory() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file("../outside.txt", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"should not be written").unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let directory = env::temp_dir().join(format!("orbit-launcher-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        assert!(extract_archive(&bytes, &directory).is_err());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(&directory).unwrap();
    }
}
