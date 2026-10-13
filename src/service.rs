use bevy::prelude::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Mutex, mpsc},
    thread,
};

#[derive(Clone)]
pub enum PendingRequest {
    Resolve(String),
    Create,
    CreateProject,
    Other,
}

#[derive(Resource)]
pub struct ServiceProcess {
    child: Child,
    input: ChildStdin,
    messages: Mutex<mpsc::Receiver<Value>>,
    next_id: u64,
    pub pending: HashMap<u64, PendingRequest>,
}

impl ServiceProcess {
    pub fn start(directory: &Path, projects: &Path) -> std::io::Result<Self> {
        let mut command = Command::new("node");
        command
            .arg(directory.join("services/host.js"))
            .arg("--projects")
            .arg(projects)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn()?;
        let input = child.stdin.take().expect("service stdin");
        let output = child.stdout.take().expect("service stdout");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                match serde_json::from_str(&line) {
                    Ok(message) => {
                        if sender.send(message).is_err() {
                            break;
                        }
                    }
                    Err(_) => eprintln!("service: {line}"),
                }
            }
            let _ = sender.send(json!({"type":"stopped"}));
        });
        Ok(Self {
            child,
            input,
            messages: Mutex::new(receiver),
            next_id: 0,
            pending: HashMap::new(),
        })
    }

    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        purpose: PendingRequest,
    ) -> Result<u64, String> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({"id":id,"method":method,"params":params});
        writeln!(self.input, "{request}").map_err(|error| error.to_string())?;
        self.input.flush().map_err(|error| error.to_string())?;
        self.pending.insert(id, purpose);
        Ok(id)
    }

    pub fn drain(&self) -> Vec<Value> {
        self.messages
            .lock()
            .map(|receiver| receiver.try_iter().collect())
            .unwrap_or_default()
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = writeln!(
            self.input,
            "{}",
            json!({"id":0,"method":"shutdown","params":{}})
        );
        let _ = self.input.flush();
        for _ in 0..200 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
