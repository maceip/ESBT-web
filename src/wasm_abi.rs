//! C ABI used by the editor page.

use crate::op::Op;
use crate::replica::{Replica, ReplicaConfig};
use crate::snapshot::{Message, Snapshot};
use crate::verify;
use std::cell::RefCell;

const MAX: usize = 64;

struct Session {
    cfg: ReplicaConfig,
    replicas: Vec<Option<Replica>>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = RefCell::new(None);
    static LAST: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

fn with<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|s| s.borrow_mut().as_mut().map(f))
}

fn store(b: Vec<u8>) -> i32 {
    LAST.with(|l| {
        let n = b.len() as i32;
        *l.borrow_mut() = b;
        n
    })
}

fn store_str(s: &str) -> i32 {
    store(s.as_bytes().to_vec())
}

fn replica<R>(site: u32, f: impl FnOnce(&mut Replica) -> R) -> Option<R> {
    with(|s| s.replicas.get_mut(site as usize).and_then(|r| r.as_mut().map(f))).flatten()
}

#[no_mangle]
pub extern "C" fn esbt_malloc(n: u32) -> *mut u8 {
    let mut v = vec![0u8; n as usize];
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[no_mangle]
pub unsafe extern "C" fn esbt_free(p: *mut u8, n: u32) {
    if !p.is_null() && n > 0 {
        drop(Vec::from_raw_parts(p, n as usize, n as usize));
    }
}

#[no_mangle]
pub extern "C" fn esbt_last_len() -> i32 {
    LAST.with(|l| l.borrow().len() as i32)
}

#[no_mangle]
pub extern "C" fn esbt_last_ptr() -> *const u8 {
    LAST.with(|l| l.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn esbt_init(dmax: i32, base: u32, depth: u32) {
    let cfg = ReplicaConfig {
        dmax: if dmax <= 0 { 1 << 16 } else { dmax as i64 },
        base: if base < 2 { (1u32 << 31) - 1 } else { base },
        depth: if depth == 0 { 256 } else { depth },
    };
    SESSION.with(|s| {
        *s.borrow_mut() = Some(Session {
            cfg,
            replicas: (0..MAX).map(|_| None).collect(),
        })
    });
}

#[no_mangle]
pub extern "C" fn esbt_add_replica(site: u32) -> i32 {
    if site == 0 || site as usize >= MAX {
        return -1;
    }
    with(|s| {
        s.replicas[site as usize] = Some(Replica::new(site, s.cfg.clone()));
        site as i32
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_len(site: u32) -> i32 {
    replica(site, |r| r.len() as i32).unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_hash(site: u32) -> u32 {
    replica(site, |r| (r.hash_state() & 0xffff_ffff) as u32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn esbt_pending(site: u32) -> i32 {
    replica(site, |r| r.pending.len() as i32).unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_text(site: u32) -> i32 {
    replica(site, |r| store_str(&r.text())).unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_insert(site: u32, index: i32, ch: u32) -> i32 {
    let Some(c) = char::from_u32(ch) else {
        return -1;
    };
    replica(site, |r| {
        let op = r.local_insert(index.max(0) as usize, c);
        store(Message::Op(op).encode())
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn esbt_insert_utf8(site: u32, index: i32, ptr: *const u8, len: u32) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    let bytes = std::slice::from_raw_parts(ptr, len as usize);
    let Ok(s) = std::str::from_utf8(bytes) else {
        return -2;
    };
    replica(site, |r| {
        let ops = r.local_insert_str(index.max(0) as usize, s);
        let mut out = Vec::new();
        out.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for op in ops {
            let m = Message::Op(op).encode();
            out.extend_from_slice(&(m.len() as u32).to_le_bytes());
            out.extend_from_slice(&m);
        }
        store(out)
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_delete_range(site: u32, index: i32, n: i32) -> i32 {
    replica(site, |r| {
        let ops = r.local_delete_range(index.max(0) as usize, n.max(0) as usize);
        let mut out = Vec::new();
        out.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for op in ops {
            let m = Message::Op(op).encode();
            out.extend_from_slice(&(m.len() as u32).to_le_bytes());
            out.extend_from_slice(&m);
        }
        store(out)
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn esbt_ingest(site: u32, ptr: *const u8, len: u32) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    let buf = std::slice::from_raw_parts(ptr, len as usize);
    let Some(msg) = Message::decode(buf) else {
        return -2;
    };
    replica(site, |r| match msg {
        Message::Op(op) => {
            r.receive(op);
            0
        }
        Message::Snapshot(s) => {
            if r.len() == 0 && r.log.is_empty() {
                r.install_snapshot(&s);
            }
            1
        }
        Message::Hello { .. } | Message::Need { .. } => 2,
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_snapshot(site: u32) -> i32 {
    replica(site, |r| store(Message::Snapshot(r.snapshot()).encode())).unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_hello(site: u32) -> i32 {
    replica(site, |r| {
        store(
            Message::Hello {
                site: r.site,
                version: r.version.clone(),
            }
            .encode(),
        )
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn esbt_fill_gap(site: u32, ptr: *const u8, len: u32) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    let buf = std::slice::from_raw_parts(ptr, len as usize);
    let Some(msg) = Message::decode(buf) else {
        return -2;
    };
    let their = match msg {
        Message::Hello { version, .. } | Message::Need { version, .. } => version,
        _ => return -3,
    };
    replica(site, |r| {
        let mut ops: Vec<Op> = Vec::new();
        for (&s, &n) in &r.version.next {
            let theirs = their.observed(s);
            if n > theirs {
                ops.extend(r.ops_in_range(s, theirs + 1, n));
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for op in ops {
            let m = Message::Op(op).encode();
            out.extend_from_slice(&(m.len() as u32).to_le_bytes());
            out.extend_from_slice(&m);
        }
        store(out)
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_weights_json(site: u32) -> i32 {
    replica(site, |r| {
        let mut s = String::from("[");
        for (i, (w, ch, c)) in r.doc.atoms().into_iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let sc = w
                .sc
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let chs = match ch {
                '"' => "\\\"".into(),
                '\\' => "\\\\".into(),
                '\n' => "\\n".into(),
                x => x.to_string(),
            };
            s.push_str(&format!(
                "{{\"p\":{},\"q\":{},\"sn\":{},\"sc\":[{sc}],\"site\":{},\"c\":{c},\"ch\":\"{chs}\"}}",
                w.f.p, w.f.q, w.sn, w.site
            ));
        }
        s.push(']');
        store_str(&s)
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn esbt_verify() -> i32 {
    let (p, f, log) = verify::run_all();
    store_str(&format!("pass={p} fail={f}\n{log}"));
    if f == 0 {
        p as i32
    } else {
        -(f as i32)
    }
}

#[allow(dead_code)]
fn _use_snapshot(_: &Snapshot) {}
