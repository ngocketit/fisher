// Copyright (C) 2016-2017 Pietro Albini
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::fmt;

use common::prelude::*;
use common::state::{State, IdKind, UniqueId};

use super::scheduled_job::ScheduledJob;
use super::scheduler::SchedulerInternalApi;
use super::types::{ScriptId, JobContext};


pub enum ProcessResult<S: ScriptsRepositoryTrait + 'static> {
    Rejected(ScheduledJob<S>),
    Executing,
}


pub struct Thread<S: ScriptsRepositoryTrait + 'static> {
    id: UniqueId,
    handle: thread::JoinHandle<()>,

    currently_running: Option<ScriptId<S>>,

    should_stop: Arc<AtomicBool>,
    communication: Arc<Mutex<Option<ScheduledJob<S>>>>,
}

impl<S: ScriptsRepositoryTrait> Thread<S> {

    pub fn new(
        processor: SchedulerInternalApi<S>,
        ctx: Arc<JobContext<S>>,
        state: &Arc<State>,
    ) -> Self {
        let thread_id = state.next_id(IdKind::ThreadId);
        let should_stop = Arc::new(AtomicBool::new(false));
        let communication = Arc::new(Mutex::new(None));

        let c_thread_id = thread_id.clone();
        let c_should_stop = should_stop.clone();
        let c_communication = communication.clone();

        let handle = thread::spawn(move || {
            let result = Thread::inner_thread(
                c_thread_id, c_should_stop, processor, c_communication, ctx,
            );

            if let Err(error) = result {
                error.pretty_print();
            }
        });

        Thread {
            id: thread_id,
            handle,

            currently_running: None,

            should_stop,
            communication,
        }
    }

    fn inner_thread(
        thread_id: UniqueId,
        should_stop: Arc<AtomicBool>,
        api: SchedulerInternalApi<S>,
        comm: Arc<Mutex<Option<ScheduledJob<S>>>>,
        ctx: Arc<JobContext<S>>,
    ) -> Result<()>{

        loop {
            // Ensure the thread is stopped
            if should_stop.load(Ordering::SeqCst) {
                break;
            }

            if let Some(job) = comm.lock()?.take() {
                let result = job.execute(&ctx);

                match result {
                    Ok(output) => {
                        api.record_output(output)?;
                    },
                    Err(error) => {
                        error.pretty_print();
                    }
                }

                api.job_ended(thread_id, &job)?;

                // Don't park the thread, look for another job right away
                continue;
            }

            // Block the thread until a new job is available
            // This avoids wasting unnecessary resources
            thread::park();
        }

        Ok(())
    }

    pub fn process(&mut self, job: ScheduledJob<S>) -> ProcessResult<S> {
        // Reject the job if the thread is going to be stopped
        if self.should_stop.load(Ordering::SeqCst) {
            return ProcessResult::Rejected(job);
        }

        if self.busy() {
            return ProcessResult::Rejected(job);
        }

        if let Ok(mut mutex) = self.communication.lock() {
            // Update the currently running ID
            self.currently_running = Some(job.hook_id());

            // Tell the thread what job it should process
            *mutex = Some(job);

            // Wake the thread up
            self.handle.thread().unpark();

            return ProcessResult::Executing;
        }

        return ProcessResult::Rejected(job);
    }

    pub fn stop(self) {
        // Tell the thread to stop and wake it up
        self.should_stop.store(true, Ordering::SeqCst);
        self.handle.thread().unpark();

        // Wait for the thread to quit
        let _ = self.handle.join();
    }

    pub fn id(&self) -> UniqueId {
        self.id
    }

    pub fn currently_running(&self) -> Option<ScriptId<S>> {
        self.currently_running
    }

    pub fn busy(&self) -> bool {
        self.currently_running.is_some()
    }

    pub fn mark_idle(&mut self) {
        self.currently_running = None;
    }
}

impl<S: ScriptsRepositoryTrait> fmt::Debug for Thread<S> {

    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Thread {{ busy: {}, should_stop: {} }}",
            self.busy(),
            self.should_stop.load(Ordering::SeqCst),
        )
    }
}
