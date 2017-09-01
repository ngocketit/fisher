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

use std::sync::{Arc, mpsc};
use std::thread;
use std::fmt;
use std::ops::Deref;

use common::prelude::*;
use common::state::{State, IdKind, UniqueId};

use super::scheduled_job::ScheduledJob;
use super::scheduler::SchedulerInternalApi;
use super::types::{ScriptId, JobContext};


#[derive(Debug)]
enum ThreadInput<S: ScriptsRepositoryTrait> {
    Process(ScheduledJob<S>),
    StopSignal,
}


pub struct Thread<S: ScriptsRepositoryTrait + 'static> {
    id: UniqueId,
    currently_running: Option<ScriptId<S>>,

    should_stop: bool,

    handle: thread::JoinHandle<()>,
    input: mpsc::Sender<ThreadInput<S>>,
}

impl<S: ScriptsRepositoryTrait> Thread<S> {

    pub fn new(processor: SchedulerInternalApi<S>, ctx: Arc<JobContext<S>>,
               state: &Arc<State>) -> Self {
        let (input_send, input_recv) = mpsc::channel();
        let id = state.next_id(IdKind::ThreadId);

        let handle = thread::spawn(move || {
            for input in input_recv.iter() {
                match input {
                    // A new job should be processed
                    ThreadInput::Process(job) => {
                        let result = job.execute(ctx.deref());

                        // Display the error if there is one
                        match result {
                            Ok(output) => {
                                processor.record_output(output).unwrap();
                            },
                            Err(mut error) => {
                                error.set_location(
                                    ErrorLocation::HookProcessing(
                                        job.hook_name().to_string()
                                    )
                                );
                                error.pretty_print();
                            }
                        }

                        processor.job_ended(id, &job).unwrap();
                    },

                    // Please stop, thanks!
                    ThreadInput::StopSignal => break,
                }
            }
        });

        Thread {
            id: id,
            currently_running: None,

            should_stop: false,

            handle: handle,
            input: input_send,
        }
    }

    // Here, None equals to success, and Some(job) equals to failure
    pub fn process(&mut self, job: ScheduledJob<S>) -> Option<ScheduledJob<S>> {
        // Do some consistency checks
        if self.should_stop || self.busy() {
            return Some(job);
        }

        self.currently_running = Some(job.hook_id());
        self.input.send(ThreadInput::Process(job)).unwrap();

        None
    }

    pub fn stop(mut self) {
        self.should_stop = true;
        self.input.send(ThreadInput::StopSignal).unwrap();

        self.handle.join().unwrap();
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
            self.busy(), self.should_stop,
        )
    }
}
