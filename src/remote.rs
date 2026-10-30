//! Everything that talks to the clusters over ssh.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::util::*;

const CONTROL_PATH: &str = "ControlPath=~/.ssh/cm-%r@%h:%p";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub temp: u32,
    pub util: u32,
    pub mem_used: u32,
    pub mem_total: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub state: String,
    pub used: Option<u64>,
    pub limit: Option<u64>,
    pub gpu_label: String,
    pub nodes: String,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub workdir: String,
    pub reason: String,
    pub partition: String,
    pub exit: String,
    pub gpus: Vec<Gpu>,
}

impl Job {
    pub fn running(&self) -> bool {
        self.state == "RUNNING"
    }
    pub fn pending(&self) -> bool {
        self.state == "PENDING"
    }
    /// Left the queue (from sacct).
    pub fn finished(&self) -> bool {
        self.end.is_some()
    }
}

pub struct Snapshot {
    pub jobs: Vec<Job>,
    /// job id -> (stdout pattern, stderr pattern), as squeue reports them
    pub outputs: HashMap<String, (String, String)>,
}

// Every line we read starts with @@, so login banners are ignored.
// The slow commands run side by side; their output goes to temp files so lines never interleave.
const JOBS_SCRIPT: &str = r#"
t=$(mktemp -d)
echo "@@NOW|$(date +%s)|$(date +%z)"
sacct -X -n -P -S now-24hours \
  -o JobID,JobName,State,Elapsed,Timelimit,End,ExitCode,WorkDir,AllocTRES,NodeList,Partition 2>/dev/null \
  | sed 's/^/@@DONE|/' > "$t/done" &
squeue -u "$USER" -h -O 'JobID:|,STDOUT:|,STDERR:' 2>/dev/null | sed 's/^/@@OUT|/' > "$t/out" &
jobs=$(squeue -u "$USER" -h -o '@@JOB|%i|%j|%T|%M|%l|%b|%N|%S|%Z|%r|%P')
printf '%s\n' "$jobs"
wait
cat "$t"/* 2>/dev/null
rm -rf "$t"
"#;

// Run separately from JOBS_SCRIPT, so slow GPU nodes don't hold up the job list.
// The job ids come after the script as arguments.
const GPUS_SCRIPT: &str = r#"
t=$(mktemp -d)
for id in "$@"; do
  timeout 10 srun --jobid="$id" --overlap -N1 -n1 \
    nvidia-smi --query-gpu=name,temperature.gpu,utilization.gpu,memory.used,memory.total \
    --format=csv,noheader,nounits </dev/null 2>/dev/null | sed "s/^/@@GPU|$id|/" > "$t/gpu.$id" &
done
wait
cat "$t"/* 2>/dev/null
rm -rf "$t"
"#;

/// Time limits, so a hung connection shows an error instead of loading forever.
const CHECK_LIMIT: Duration = Duration::from_secs(5);
const CONNECT_LIMIT: Duration = Duration::from_secs(30);
const RUN_LIMIT: Duration = Duration::from_secs(60);

/// Wait for a child process, killing it if it takes longer than `limit`.
fn wait_limit(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Make sure a shared ssh connection is up, so each poll skips the login handshake.
/// The master is started with no pipes attached, otherwise it would hold our output open.
fn ensure_master(host: &str) -> Result<(), String> {
    let mut check = Command::new("ssh")
        .args(["-o", CONTROL_PATH, "-O", "check", host])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    match wait_limit(&mut check, CHECK_LIMIT) {
        Some(s) if s.success() => return Ok(()),
        Some(_) => {}
        // the shared connection is wedged; keep-alives (below) make it exit on its own soon
        None => return Err("ssh connection is stuck, retrying…".into()),
    }
    let errpath = std::env::temp_dir().join(format!("gpudash-{host}.err"));
    let errfile = File::create(&errpath).map_err(|e| e.to_string())?;
    let mut master = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=10",
            "-o", "ControlMaster=yes",
            "-o", "ControlPersist=10m",
            // notice a dead connection within ~45s and close it, so the next poll reconnects
            "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=3",
            "-o", CONTROL_PATH,
            "-fN", host,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errfile)
        .spawn()
        .map_err(|e| e.to_string())?;
    match wait_limit(&mut master, CONNECT_LIMIT) {
        Some(s) if s.success() => return Ok(()),
        Some(_) => {}
        None => return Err(format!("couldn't connect within {}s", CONNECT_LIMIT.as_secs())),
    }
    let mut err = String::new();
    let _ = File::open(&errpath).and_then(|mut f| f.read_to_string(&mut err));
    Err(last_line(&err).unwrap_or("ssh failed").to_string())
}

/// Run a bash script on the login node and return its stdout.
fn run(host: &str, script: &str) -> Result<String, String> {
    ensure_master(host)?;
    let mut child = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ControlMaster=no", "-o", CONTROL_PATH, host, "bash -s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().unwrap().write_all(script.as_bytes()).map_err(|e| e.to_string())?;
    // read both pipes on their own threads so a full pipe can't stall the process
    let read = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out_reader = read(Box::new(child.stdout.take().unwrap()));
    let err_reader = read(Box::new(child.stderr.take().unwrap()));
    if wait_limit(&mut child, RUN_LIMIT).is_none() {
        return Err(format!("no answer within {}s, will retry", RUN_LIMIT.as_secs()));
    }
    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !stdout.contains("@@") {
        return Err(last_line(&stderr).unwrap_or("no output").to_string());
    }
    Ok(stdout)
}

fn last_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).last()
}

fn parse_time(s: &str, tz: FixedOffset) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .and_then(|t| tz.from_local_datetime(&t).single())
        .map(|t| t.with_timezone(&Utc))
}

pub fn fetch_jobs(host: &str) -> Result<Snapshot, String> {
    let out = run(host, JOBS_SCRIPT)?;
    let mut tz = FixedOffset::east_opt(0).unwrap();
    let mut active: Vec<Job> = Vec::new();
    let mut done: Vec<Job> = Vec::new();
    let mut outputs = HashMap::new();

    for line in out.lines().filter(|l| l.starts_with("@@")) {
        let f: Vec<&str> = line.split('|').collect();
        match f[0] {
            "@@NOW" if f.len() >= 3 => tz = parse_offset(f[2]).unwrap_or(tz),
            "@@JOB" if f.len() >= 12 => active.push(Job {
                id: f[1].into(),
                name: f[2].into(),
                state: f[3].into(),
                used: slurm_secs(f[4]),
                limit: slurm_secs(f[5]),
                gpu_label: gpu_from_gres(f[6]),
                nodes: f[7].into(),
                start: parse_time(f[8], tz),
                end: None,
                workdir: f[9].into(),
                reason: f[10].into(),
                partition: f[11].into(),
                exit: String::new(),
                gpus: Vec::new(),
            }),
            "@@DONE" if f.len() >= 12 => {
                // 'CANCELLED by 1234' -> 'CANCELLED'
                let state = f[3].split_whitespace().next().unwrap_or("").to_string();
                let Some(end) = parse_time(f[6], tz) else { continue };
                if matches!(state.as_str(), "PENDING" | "RUNNING" | "REQUEUED" | "RESIZING" | "SUSPENDED") {
                    continue;
                }
                done.push(Job {
                    id: f[1].into(),
                    name: f[2].into(),
                    state,
                    used: slurm_secs(f[4]),
                    limit: slurm_secs(f[5]),
                    gpu_label: gpu_from_tres(f[9]),
                    nodes: f[10].into(),
                    start: None,
                    end: Some(end),
                    workdir: f[8].into(),
                    reason: String::new(),
                    partition: f[11].into(),
                    exit: f[7].into(),
                    gpus: Vec::new(),
                });
            }
            "@@OUT" if f.len() >= 4 => {
                outputs.insert(f[1].trim().to_string(), (f[2].trim().to_string(), f[3].trim().to_string()));
            }
            _ => {}
        }
    }
    let rank = |s: &str| match s {
        "RUNNING" => 0,
        "CONFIGURING" => 1,
        "COMPLETING" => 2,
        _ => 3,
    };
    active.sort_by_key(|j| (rank(&j.state), j.start.is_none(), j.start));
    done.retain(|d| !active.iter().any(|a| a.id == d.id));
    done.sort_by_key(|j| std::cmp::Reverse(j.end));
    active.extend(done);
    Ok(Snapshot { jobs: active, outputs })
}

/// Read the GPUs of running jobs: job id -> its GPUs.
pub fn fetch_gpus(host: &str, ids: &[String]) -> Result<HashMap<String, Vec<Gpu>>, String> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let args: Vec<String> = ids.iter().map(|id| sh_quote(id)).collect();
    let script = format!("set -- {}\n{GPUS_SCRIPT}", args.join(" "));
    Ok(parse_gpus(&run(host, &script)?))
}

fn parse_gpus(out: &str) -> HashMap<String, Vec<Gpu>> {
    let mut gpus: HashMap<String, Vec<Gpu>> = HashMap::new();
    for line in out.lines() {
        let Some((id, csv)) = line.strip_prefix("@@GPU|").and_then(|l| l.split_once('|')) else { continue };
        let p: Vec<&str> = csv.rsplitn(5, ',').map(str::trim).collect();
        if p.len() == 5 {
            let n = |s: &str| s.parse().unwrap_or(0);
            gpus.entry(id.into()).or_default().push(Gpu {
                name: short_gpu_name(p[4]),
                temp: n(p[3]),
                util: n(p[2]),
                mem_used: n(p[1]),
                mem_total: n(p[0]),
            });
        }
    }
    gpus
}

#[derive(Debug)]
pub struct Log {
    pub path: String,
    pub size: u64,
    pub lines: Vec<String>,
    pub stamp: String,
}

/// Inputs for resolving and tailing a job's stdout or stderr.
pub struct LogRequest<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub workdir: &'a str,
    /// Fallback path pattern saved from squeue, used once Slurm forgets the job.
    pub pattern: &'a str,
    pub stderr: bool,
    /// Last displayed path and file metadata; None forces a full read.
    pub known: Option<(&'a str, &'a str)>,
}

impl LogRequest<'_> {
    fn script(&self) -> String {
        let (path, stamp) = self.known.unwrap_or_default();
        format!(
            r#"
id={id}; name={name}; pat={pat}; key={key}; wd={wd}
known_path={path}; known_stamp={stamp}
p=$(scontrol show job -o "$id" 2>/dev/null | tr ' ' '\n' | sed -n "s/^$key=//p")
case "$p" in ""|"(null)") p=$(sacct -j "$id" -X -n -P -o "$key" 2>/dev/null | head -1);; esac
case "$p" in ""|"(null)") p="$pat";; esac
A=${{id%%_*}}; a=${{id#*_}}; [ "$a" = "$id" ] && a=""
p=${{p//\%j/$id}}; p=${{p//\%x/$name}}; p=${{p//\%u/$USER}}; p=${{p//\%A/$A}}; p=${{p//\%a/$a}}
# last resort: a file named after the job id somewhere in the job's folder
if [ -z "$p" ] && [ -d "$wd" ]; then
  p=$(find "$wd" -maxdepth 3 -type f -name "*$A*" \( -name '*.out' -o -name '*.log' -o -name '*.err' -o -name '*.txt' \) 2>/dev/null \
      | grep -E "$([ $key = StdErr ] && echo 'err' || echo 'out|log|txt')" | head -1)
fi
echo "@@PATH|$p"
if [ -z "$p" ]; then echo "@@ERR|Slurm no longer knows where this job's log is"
elif [ ! -r "$p" ]; then echo "@@ERR|can't read $p"
else
  stamp=$(stat -c '%s|%i|%y|%z' "$p" 2>/dev/null)
  if [ -n "$stamp" ] && [ "$p" = "$known_path" ] && [ "$stamp" = "$known_stamp" ]; then
    echo "@@UNCHANGED"
  else
    echo "@@STAMP|$stamp"
    echo "@@SIZE|${{stamp%%|*}}"
    tail -n 3000 "$p"
  fi
fi
"#,
            id = sh_quote(self.id),
            name = sh_quote(self.name),
            pat = sh_quote(self.pattern),
            key = if self.stderr { "StdErr" } else { "StdOut" },
            wd = sh_quote(self.workdir),
            path = sh_quote(path),
            stamp = sh_quote(stamp),
        )
    }
}

/// None means the displayed file is unchanged; keep its existing lines.
pub fn fetch_log(host: &str, request: &LogRequest<'_>) -> Result<Option<Log>, String> {
    parse_log(&run(host, &request.script())?)
}

fn parse_log(out: &str) -> Result<Option<Log>, String> {
    let mut stamp = String::new();
    let mut path = String::new();
    let mut lines = out.lines();
    for line in lines.by_ref() {
        if let Some(p) = line.strip_prefix("@@PATH|") {
            path = p.to_string();
        } else if line == "@@UNCHANGED" {
            return Ok(None);
        } else if let Some(s) = line.strip_prefix("@@STAMP|") {
            stamp = s.to_string();
        } else if let Some(e) = line.strip_prefix("@@ERR|") {
            return Err(e.to_string());
        } else if let Some(s) = line.strip_prefix("@@SIZE|") {
            let size = s.trim().parse().unwrap_or(0);
            let lines = lines.map(clean_log_line).collect();
            return Ok(Some(Log { path, size, lines, stamp }));
        }
    }
    Err("no log output".into())
}

pub fn cancel(host: &str, id: &str) -> Result<(), String> {
    let script = format!("scancel {} 2>&1 && echo @@OK || echo @@FAIL", sh_quote(id));
    let out = run(host, &script)?;
    if out.contains("@@OK") {
        Ok(())
    } else {
        let msg = out.lines().filter(|l| !l.starts_with("@@")).map(str::trim).filter(|l| !l.is_empty()).last();
        Err(msg.unwrap_or("scancel failed").to_string())
    }
}

/// Interactive shell: inside the job's allocation if it's running, else in its folder on the login node.
/// Close the shared ssh connection, so the next poll logs in fresh.
pub fn disconnect(host: &str) {
    let _ = Command::new("ssh")
        .args(["-o", CONTROL_PATH, "-O", "exit", host])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

pub fn shell(host: &str, workdir: &str, running_id: Option<&str>) -> std::io::Result<()> {
    let cd = format!("cd {} 2>/dev/null", sh_quote(workdir));
    let remote = match running_id {
        Some(id) => format!("{cd}; exec srun --jobid={} --overlap --pty bash -l", sh_quote(id)),
        None => format!("{cd}; exec bash -l"),
    };
    Command::new("ssh")
        .args(["-t", "-o", "ControlMaster=no", "-o", CONTROL_PATH, host, &remote])
        .status()
        .map(|_| ())
}

pub fn copy_to_clipboard(text: &str) -> bool {
    let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() else { return false };
    let ok = child.stdin.take().is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|s| s.success()) && ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(script: &str) -> String {
        let out = Command::new("bash").args(["-c", script]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn log_polls_skip_only_the_same_file_metadata() {
        let read = |contents: &str, stamp: &str, known, pattern: &str, forbid_tail| {
            let request = LogRequest {
                id: "42", name: "train", workdir: "", pattern, stderr: false, known,
            };
            let out = bash(&format!(r#"
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
cd "$t"
printf '%s' {contents} > train.log
scontrol() {{ :; }}
sacct() {{ :; }}
stat() {{ printf '%s\n' {stamp}; }}
tail() {{ {tail} }}
{script}"#,
                contents = sh_quote(contents), stamp = sh_quote(stamp),
                tail = if forbid_tail { "echo 'tail must not run' >&2; exit 1;" } else { r#"command tail "$@";"# },
                script = request.script(),
            ));
            parse_log(&out)
        };
        let stamp = "4|123|mtime|ctime";
        let first = read("old\n", stamp, None, "train.log", false).unwrap().unwrap();
        assert_eq!(first.lines, ["old"]);
        assert_eq!(first.size, 4);
        let known = Some((first.path.as_str(), first.stamp.as_str()));
        assert!(read("old\n", stamp, known, "train.log", true).unwrap().is_none());
        let appended = read("old\nnew\n", "8|123|later|later", known, "train.log", false).unwrap().unwrap();
        assert_eq!(appended.lines, ["old", "new"]);
        let rewritten = read("new\n", "4|123|later|later", known, "train.log", false).unwrap().unwrap();
        assert_eq!(rewritten.lines, ["new"]);
        let truncated = read("", "0|123|later|later", known, "train.log", false).unwrap().unwrap();
        assert!(truncated.lines.is_empty());
        // A new path with matching metadata must still be read.
        assert!(read("old\n", stamp, Some(("other.log", stamp)), "train.log", false).unwrap().is_some());
        // No known metadata means a manual or final read always transfers the lines.
        assert!(read("old\n", stamp, None, "train.log", false).unwrap().is_some());
        assert!(read("old\n", stamp, known, "missing.log", true).unwrap_err().contains("can't read"));
        // If stat fails, fall back to tailing instead of treating it as unchanged.
        assert!(read("old\n", "", Some(("train.log", "")), "train.log", false).unwrap().is_some());
    }

    #[test]
    fn jobs_script_does_not_probe_gpus() {
        let out = bash(&format!(r#"
squeue() {{ case "$*" in *STDOUT*) ;; *) echo '@@JOB|42|train|RUNNING|1:00|1:00:00|gpu:1|node1|2026-10-30T12:00:00|/tmp|None|gpu';; esac; }}
sacct() {{ :; }}
srun() {{ echo UNEXPECTED; }}
{JOBS_SCRIPT}"#));
        assert!(!out.contains("UNEXPECTED"));
        assert!(out.contains("@@JOB|42|"));
    }

    #[test]
    fn gpus_script_reads_each_job() {
        let out = bash(&format!(r#"
set -- 42 '43_1' 44
timeout() {{ shift; "$@"; }}
srun() {{
  case "$1" in
    --jobid=42) printf 'NVIDIA H200, 50, 90, 100, 200\nNVIDIA H200, 51, 91, 101, 200\n';;
    --jobid=43_1) echo 'NVIDIA A100, 60, 80, 150, 200';;
    *) return 1;;
  esac
}}
{GPUS_SCRIPT}"#));
        let gpus = parse_gpus(&out);
        assert_eq!(gpus["42"].len(), 2);
        assert_eq!(gpus["42"][1].temp, 51);
        assert_eq!(gpus["43_1"][0].util, 80);
        assert!(!gpus.contains_key("44"));
    }
}
