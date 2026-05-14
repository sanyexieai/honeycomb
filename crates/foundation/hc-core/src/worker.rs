use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use crate::{JobKind, JobRecord, JobState, WorkerReport};

#[derive(Debug, thiserror::Error)]
pub enum WorkerExecutionError {
    #[error("process worker only supports process jobs")]
    UnsupportedJobKind,
    #[error("failed to launch process: {0}")]
    Spawn(#[from] std::io::Error),
}

pub struct ProcessWorker;

impl ProcessWorker {
    pub fn execute(job: &JobRecord) -> Result<Vec<WorkerReport>, WorkerExecutionError> {
        let mut reports = Vec::new();
        Self::execute_streaming(job, |report| reports.push(report))?;
        Ok(reports)
    }

    pub fn execute_streaming<F>(
        job: &JobRecord,
        mut on_report: F,
    ) -> Result<(), WorkerExecutionError>
    where
        F: FnMut(WorkerReport),
    {
        let mut handle = Self::start(job)?;
        for report in handle.take_startup_reports() {
            on_report(report);
        }
        for report in handle.wait()? {
            on_report(report);
        }

        Ok(())
    }

    pub fn start(job: &JobRecord) -> Result<ProcessWorkerHandle, WorkerExecutionError> {
        if job.kind != JobKind::Process {
            return Err(WorkerExecutionError::UnsupportedJobKind);
        }

        let mut command = Command::new(&job.run_request.program);
        command.args(&job.run_request.args);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if let Some(cwd) = &job.run_request.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout pipe was not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("stderr pipe was not captured"))?;
        let (sender, receiver) = mpsc::channel();

        spawn_report_reader(stdout, sender.clone(), job.id.clone(), StreamKind::Stdout);
        spawn_report_reader(stderr, sender, job.id.clone(), StreamKind::Stderr);

        Ok(ProcessWorkerHandle {
            job_id: job.id.clone(),
            child,
            receiver,
            pending_reports: vec![WorkerReport::JobStateChanged {
                job_id: job.id.clone(),
                state: JobState::Running,
            }],
        })
    }
}

pub struct ProcessWorkerHandle {
    job_id: String,
    child: Child,
    receiver: Receiver<WorkerReport>,
    pending_reports: Vec<WorkerReport>,
}

impl ProcessWorkerHandle {
    pub fn take_startup_reports(&mut self) -> Vec<WorkerReport> {
        std::mem::take(&mut self.pending_reports)
    }

    pub fn drain_reports(&mut self) -> Vec<WorkerReport> {
        let mut reports = self.take_startup_reports();
        loop {
            match self.receiver.try_recv() {
                Ok(report) => reports.push(report),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        reports
    }

    pub fn wait(mut self) -> Result<Vec<WorkerReport>, WorkerExecutionError> {
        let mut reports = self.drain_reports();
        let status = self.child.wait()?;

        loop {
            match self.receiver.recv() {
                Ok(report) => reports.push(report),
                Err(_) => break,
            }
        }

        reports.push(WorkerReport::Exited {
            job_id: self.job_id,
            success: status.success(),
        });

        Ok(reports)
    }

    pub fn terminate(&mut self) -> Result<(), WorkerExecutionError> {
        self.child.kill()?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

fn spawn_report_reader<R>(
    reader: R,
    sender: mpsc::Sender<WorkerReport>,
    job_id: String,
    stream_kind: StreamKind,
) where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = String::new();

        loop {
            buffer.clear();
            match reader.read_line(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    let chunk = buffer.clone();
                    let report = match stream_kind {
                        StreamKind::Stdout => WorkerReport::Stdout {
                            job_id: job_id.clone(),
                            chunk,
                        },
                        StreamKind::Stderr => WorkerReport::Stderr {
                            job_id: job_id.clone(),
                            chunk,
                        },
                    };
                    if sender.send(report).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}
