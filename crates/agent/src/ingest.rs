use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use kairos_core::event::{Event, Kind, VERSION};
use anyhow::{bail, Result};

use crate::diag::diag;
use crate::{now, Msg};

fn accept(line: &str) -> Option<Event> {
    let mut e: Event = serde_json::from_str(line).ok()?;
    let allowed = matches!(e.kind, Kind::Prompt { .. } | Kind::AgentStart { .. } | Kind::ReplyDone { .. });
    if e.v != VERSION || !allowed {
        return None;
    }
    if !e.ts.is_finite() || e.ts <= 0.0 {
        e.ts = now();
    }
    Some(e)
}

pub fn listen(sock: PathBuf, tx: Sender<Msg>) -> Result<()> {
    if sock.exists() {
        if UnixStream::connect(&sock).is_ok() {
            bail!("another kairosd is already listening on {}", sock.display());
        }
        fs::remove_file(&sock)?;
    }
    let listener = UnixListener::bind(&sock)?;
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))?;
    thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            thread::spawn(move || {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    if let Some(e) = accept(&line) {
                        let _ = tx.send(Msg::Hook(e));
                    }
                }
            });
        }
    });
    Ok(())
}

pub fn drain_spool(spool: &Path, tx: &Sender<Msg>) {
    let taken = spool.with_extension("jsonl.ingest");
    if fs::rename(spool, &taken).is_err() && !taken.exists() {
        return;
    }
    let Ok(text) = fs::read_to_string(&taken) else { return };
    let mut n = 0;
    for e in text.lines().filter_map(accept) {
        let _ = tx.send(Msg::Hook(e));
        n += 1;
    }
    let _ = fs::remove_file(&taken);
    diag!("kairosd: ingested {n} spooled events");
}
