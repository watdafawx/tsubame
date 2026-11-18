//! Sequential, cancellable filesystem work. All disk access happens on the worker.
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub enum Operation {
    Copy { source: PathBuf, target: PathBuf },
    Move { source: PathBuf, target: PathBuf },
    Rename { source: PathBuf, target: PathBuf },
    Delete { path: PathBuf, permanent: bool },
    NewFolder(PathBuf),
    NewFile(PathBuf),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Replace,
    Skip,
    KeepBoth,
}
pub struct ConflictReply(Sender<(Choice, bool)>);
impl ConflictReply {
    pub fn answer(self, choice: Choice, apply_to_all: bool) {
        let _ = self.0.send((choice, apply_to_all));
    }
}
pub enum Event {
    Started(u64),
    Progress {
        id: u64,
        bytes_done: u64,
        current: PathBuf,
    },
    Conflict {
        id: u64,
        target: PathBuf,
        reply: ConflictReply,
    },
    Error {
        id: u64,
        item: Operation,
        message: String,
    },
    Finished {
        id: u64,
        cancelled: bool,
    },
}
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        } else {
            Ok(())
        }
    }
}
pub struct Ticket {
    pub id: u64,
    pub cancellation: Cancellation,
}
/// Backends run exclusively on the queue worker. Return an inverse only after success.
pub trait OpBackend: Send + 'static {
    fn execute(
        &mut self,
        op: &Operation,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(u64),
    ) -> io::Result<Option<Operation>>;
}
#[derive(Default)]
pub struct FsBackend;
fn remove(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
fn copy(
    source: &Path,
    target: &Path,
    cancel: &Cancellation,
    progress: &mut dyn FnMut(u64),
    created: &mut bool,
) -> io::Result<()> {
    cancel.check()?;
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "reparse points require shell backend",
        ));
    }
    if meta.is_dir() {
        fs::create_dir(target)?;
        *created = true;
        for item in fs::read_dir(source)? {
            let item = item?;
            copy(
                &item.path(),
                &target.join(item.file_name()),
                cancel,
                progress,
                &mut false,
            )?;
        }
    } else {
        let mut input = fs::File::open(source)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        *created = true;
        let mut buffer = vec![0; 256 * 1024];
        loop {
            cancel.check()?;
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            progress(n as u64);
        }
        output.flush()?;
    }
    Ok(())
}
fn validate(source: &Path, target: &Path) -> io::Result<()> {
    let source = fs::canonicalize(source)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("target needs parent"))?;
    let target = fs::canonicalize(parent)?.join(
        target
            .file_name()
            .ok_or_else(|| io::Error::other("target needs name"))?,
    );
    // Windows comparisons are case insensitive; preserve spelling for case-only rename.
    let s = source.to_string_lossy().to_lowercase();
    let t = target.to_string_lossy().to_lowercase();
    if s == t || (source.is_dir() && Path::new(&t).starts_with(Path::new(&s))) {
        return Err(io::Error::other(
            "source equals target or target is inside source",
        ));
    }
    Ok(())
}
impl OpBackend for FsBackend {
    fn execute(
        &mut self,
        op: &Operation,
        cancel: &Cancellation,
        progress: &mut dyn FnMut(u64),
    ) -> io::Result<Option<Operation>> {
        cancel.check()?;
        match op {
            Operation::Copy { source, target }
            | Operation::Move { source, target }
            | Operation::Rename { source, target } => {
                let case_rename = matches!(op, Operation::Rename { .. })
                    && source != target
                    && source
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&target.to_string_lossy());
                if !case_rename {
                    validate(source, target)?;
                    if target.exists() {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "target exists",
                        ));
                    }
                }
                if !matches!(op, Operation::Copy { .. }) {
                    match fs::rename(source, target) {
                        Ok(()) => {
                            return Ok(Some(Operation::Move {
                                source: target.clone(),
                                target: source.clone(),
                            }));
                        }
                        Err(e) if e.raw_os_error() == Some(17) => {}
                        Err(e) => return Err(e),
                    }
                }
                if target.exists() && !case_rename {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "target exists",
                    ));
                }
                let mut created = false;
                if let Err(e) = copy(source, target, cancel, progress, &mut created) {
                    if created {
                        let _ = remove(target);
                    }
                    return Err(e);
                }
                if !matches!(op, Operation::Copy { .. }) {
                    remove(source)?;
                    return Ok(Some(Operation::Move {
                        source: target.clone(),
                        target: source.clone(),
                    }));
                }
                Ok(Some(Operation::Delete {
                    path: target.clone(),
                    permanent: true,
                }))
            }
            Operation::Delete {
                path,
                permanent: true,
            } => {
                remove(path)?;
                Ok(None)
            }
            Operation::Delete {
                permanent: false, ..
            } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "recycling requires shell backend",
            )),
            Operation::NewFolder(path) => {
                fs::create_dir(path)?;
                Ok(Some(Operation::Delete {
                    path: path.clone(),
                    permanent: true,
                }))
            }
            Operation::NewFile(path) => {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?;
                Ok(Some(Operation::Delete {
                    path: path.clone(),
                    permanent: true,
                }))
            }
        }
    }
}
struct Job {
    id: u64,
    cancel: Cancellation,
    command: Command,
}
enum Command {
    Run(Vec<Operation>),
    Undo,
    Redo,
}
#[derive(Clone)]
struct Record {
    forward: Operation,
    inverse: Operation,
}
/// Submission never waits for disk or a conflict response. Dropping detaches the worker.
pub struct FileOpQueue {
    tx: Sender<Job>,
    next: std::sync::atomic::AtomicU64,
}
impl FileOpQueue {
    pub fn new<B: OpBackend>(mut backend: B) -> io::Result<(Self, Receiver<Event>)> {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (events, rx) = mpsc::channel();
        std::thread::Builder::new().name("tsubame-fileops".into()).spawn(move || {
            use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
            let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            let mut undo = Vec::<Record>::new(); let mut redo = Vec::<Record>::new();
            for job in jobs {
                let _ = events.send(Event::Started(job.id));
                let (ops, mode) = match job.command {
                    Command::Run(ops) => (ops, 0),
                    Command::Undo => (undo.last().map(|r| vec![r.inverse.clone()]).unwrap_or_default(), 1),
                    Command::Redo => (redo.last().map(|r| vec![r.forward.clone()]).unwrap_or_default(), 2),
                };
                let mut all = None; let mut last = Instant::now();
                for mut op in ops {
                    if job.cancel.is_cancelled() { break; }
                    let case_rename = matches!(&op, Operation::Rename { source, target } if source != target && source.to_string_lossy().eq_ignore_ascii_case(&target.to_string_lossy()));
                    let target = match &mut op { Operation::Copy { target, .. } | Operation::Move { target, .. } | Operation::Rename { target, .. } => Some(target), _ => None };
                    if let Some(target) = target && target.exists() && !case_rename {
                            let choice = if let Some(c) = all { c } else {
                                let (tx, rx) = mpsc::channel();
                                if events.send(Event::Conflict { id: job.id, target: target.clone(), reply: ConflictReply(tx) }).is_err() { job.cancel.cancel(); break; }
                                let answer = loop { match rx.recv_timeout(Duration::from_millis(20)) { Ok(a) => break Some(a), Err(mpsc::RecvTimeoutError::Timeout) if !job.cancel.is_cancelled() => continue, _ => break None } };
                                let Some((c, apply)) = answer else { job.cancel.cancel(); break; }; if apply { all = Some(c); } c
                            };
                            match choice {
                                Choice::Skip => continue,
                                Choice::Replace => { let _ = events.send(Event::Error { id: job.id, item: op.clone(), message: "Replace requires transactional backup; refusing data loss".into() }); continue; }
                                Choice::KeepBoth => { let original = target.clone(); let mut n = 2; while target.exists() { let mut name = original.file_stem().unwrap_or_default().to_os_string(); name.push(format!(" ({n})")); if let Some(ext) = original.extension() { name.push("."); name.push(ext); } *target = original.with_file_name(name); n += 1; } }
                            }
                    }
                    let mut done = 0;
                    let result = backend.execute(&op, &job.cancel, &mut |n| { done += n; if last.elapsed() >= Duration::from_nanos(33_333_334) { let _ = events.send(Event::Progress { id: job.id, bytes_done: done, current: match &op { Operation::Copy { source, .. } | Operation::Move { source, .. } | Operation::Rename { source, .. } => source.clone(), _ => PathBuf::new() } }); last = Instant::now(); } });
                    match result {
                        Ok(inverse) => match mode {
                            1 => { if let Some(r) = undo.pop() { redo.push(r); } }
                            2 => { if let Some(r) = redo.pop() { undo.push(r); } }
                            _ => { redo.clear(); if let Some(inverse) = inverse { undo.push(Record { forward: op, inverse }); if undo.len() > 200 { undo.remove(0); } } }
                        },
                        Err(e) => { let _ = events.send(Event::Error { id: job.id, item: op, message: e.to_string() }); }
                    }
                }
                let _ = events.send(Event::Finished { id: job.id, cancelled: job.cancel.is_cancelled() });
            }
            if initialized { unsafe { CoUninitialize(); } }
        })?;
        Ok((
            Self {
                tx,
                next: std::sync::atomic::AtomicU64::new(1),
            },
            rx,
        ))
    }
    fn send(&self, command: Command) -> io::Result<Ticket> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let cancellation = Cancellation::default();
        self.tx
            .send(Job {
                id,
                cancel: cancellation.clone(),
                command,
            })
            .map_err(|_| io::Error::other("queue closed"))?;
        Ok(Ticket { id, cancellation })
    }
    pub fn submit(&self, operations: Vec<Operation>) -> io::Result<Ticket> {
        self.send(Command::Run(operations))
    }
    pub fn undo(&self) -> io::Result<Ticket> {
        self.send(Command::Undo)
    }
    pub fn redo(&self) -> io::Result<Ticket> {
        self.send(Command::Redo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "tsubame-fileops-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn finish(rx: &Receiver<Event>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                Event::Finished { .. } => break,
                Event::Error { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
    }
    #[test]
    fn queue_copy_undo_redo() {
        let t = Temp::new();
        let a = t.0.join("a");
        let b = t.0.join("b");
        fs::write(&a, b"hello").unwrap();
        let (q, rx) = FileOpQueue::new(FsBackend).unwrap();
        q.submit(vec![Operation::Copy {
            source: a,
            target: b.clone(),
        }])
        .unwrap();
        finish(&rx);
        assert_eq!(fs::read(&b).unwrap(), b"hello");
        q.undo().unwrap();
        finish(&rx);
        assert!(!b.exists());
        q.redo().unwrap();
        finish(&rx);
        assert!(b.exists());
    }
    #[test]
    fn nested_move_rename_delete_and_self_rejection() {
        let t = Temp::new();
        let a = t.0.join("a");
        fs::create_dir(&a).unwrap();
        fs::write(a.join("file"), b"x").unwrap();
        let mut b = FsBackend;
        let c = Cancellation::default();
        assert!(
            b.execute(
                &Operation::Move {
                    source: a.clone(),
                    target: a.join("inside")
                },
                &c,
                &mut |_| {}
            )
            .is_err()
        );
        let dest = t.0.join("b");
        b.execute(
            &Operation::Move {
                source: a.clone(),
                target: dest.clone(),
            },
            &c,
            &mut |_| {},
        )
        .unwrap();
        assert!(!a.exists());
        let upper = t.0.join("B");
        b.execute(
            &Operation::Rename {
                source: dest,
                target: upper.clone(),
            },
            &c,
            &mut |_| {},
        )
        .unwrap();
        b.execute(
            &Operation::Delete {
                path: upper.clone(),
                permanent: true,
            },
            &c,
            &mut |_| {},
        )
        .unwrap();
        assert!(!upper.exists());
    }
    #[test]
    fn cancel_copy_removes_partial_file() {
        let t = Temp::new();
        let a = t.0.join("a");
        let b = t.0.join("b");
        let f = fs::File::create(&a).unwrap();
        f.set_len(64 * 1024 * 1024).unwrap();
        let c = Cancellation::default();
        let result = FsBackend.execute(
            &Operation::Copy {
                source: a,
                target: b.clone(),
            },
            &c,
            &mut |_| c.cancel(),
        );
        assert!(result.is_err());
        assert!(!b.exists());
    }
    #[test]
    fn keep_both_apply_all_and_skip() {
        let t = Temp::new();
        let a = t.0.join("a");
        let b = t.0.join("b.txt");
        fs::write(&a, b"new").unwrap();
        fs::write(&b, b"old").unwrap();
        let (q, rx) = FileOpQueue::new(FsBackend).unwrap();
        let op = Operation::Copy {
            source: a,
            target: b.clone(),
        };
        q.submit(vec![op.clone(), op.clone()]).unwrap();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::Conflict { reply, .. } => reply.answer(Choice::KeepBoth, true),
                Event::Error { message, .. } => panic!("{message}"),
                Event::Finished { .. } => break,
                _ => {}
            }
        }
        assert!(t.0.join("b (2).txt").exists());
        assert!(t.0.join("b (3).txt").exists());
        q.submit(vec![op]).unwrap();
        loop {
            match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                Event::Conflict { reply, .. } => reply.answer(Choice::Skip, false),
                Event::Finished { .. } => break,
                _ => {}
            }
        }
        assert_eq!(fs::read(b).unwrap(), b"old");
    }
}

#[cfg(test)]
mod benchmarks {
    use super::*;
    struct Fake;
    impl OpBackend for Fake {
        fn execute(
            &mut self,
            _: &Operation,
            _: &Cancellation,
            _: &mut dyn FnMut(u64),
        ) -> io::Result<Option<Operation>> {
            Ok(None)
        }
    }
    #[test]
    fn fake_queue_overhead() {
        let (q, rx) = FileOpQueue::new(Fake).unwrap();
        let ops = (0..100_000)
            .map(|_| Operation::NewFile(PathBuf::new()))
            .collect();
        let start = Instant::now();
        q.submit(ops).unwrap();
        while !matches!(
            rx.recv_timeout(Duration::from_secs(30)).unwrap(),
            Event::Finished { .. }
        ) {}
        println!(
            "fake queue: {:.3} us/item",
            start.elapsed().as_secs_f64() * 1e6 / 100_000.0
        );
    }
    #[test]
    fn small_file_throughput() {
        let root = std::env::temp_dir().join(format!("tsubame-10k-{}", std::process::id()));
        assert!(!root.exists());
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        let target = root.join("copy");
        fs::create_dir(&source).unwrap();
        for i in 0..10_000 {
            fs::write(source.join(format!("{i}.txt")), [42; 128]).unwrap();
        }
        let start = Instant::now();
        FsBackend
            .execute(
                &Operation::Copy {
                    source: source.clone(),
                    target: target.clone(),
                },
                &Cancellation::default(),
                &mut |_| {},
            )
            .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(fs::read_dir(target).unwrap().count(), 10_000);
        println!(
            "fs copy: {:.3}s, {:.0} files/s",
            elapsed.as_secs_f64(),
            10_000.0 / elapsed.as_secs_f64()
        );
        let start = Instant::now();
        let status = std::process::Command::new("robocopy")
            .arg(&source)
            .arg(root.join("robocopy"))
            .args(["/E", "/NFL", "/NDL", "/NJH", "/NJS", "/NP", "/R:0", "/W:0"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        let elapsed = start.elapsed();
        assert!(status.code().unwrap() < 8);
        println!(
            "robocopy: {:.3}s, {:.0} files/s",
            elapsed.as_secs_f64(),
            10_000.0 / elapsed.as_secs_f64()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
