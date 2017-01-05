// Copyright (C) 2016 Pietro Albini
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

use std::collections::HashMap;
use std::time::Duration;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::fs;

use hyper::client as hyper;
use hyper::method::Method;

use app::FisherOptions;
use hooks::{self, Hooks};
use jobs::Job;
use web::WebApp;
use requests::Request;
use processor::{ProcessorInput, HealthDetails};
use providers::{Provider, ProviderFactory, BoxedProvider, testing};
use errors::FisherResult;
use utils;

use utils::compat;


#[macro_export]
macro_rules! assert_err {
    ($result:expr, $pattern:pat) => {{
        match $result {
            Ok(..) => {
                panic!("{} didn't error out",
                    stringify!($result)
                );
            },
            Err(error) => {
                match *error.kind() {
                    $pattern => {},
                    _ => {
                        panic!("{} didn't error with {}",
                            stringify!($result),
                            stringify!($pattern)
                        );
                    },
                }
            },
        }
    }};
}


pub fn dummy_request() -> Request {
    Request {
        headers: HashMap::new(),
        params: HashMap::new(),
        source: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        body: String::new(),
    }
}


pub fn testing_provider_factory() -> ProviderFactory {
    fn factory(config: &str) -> FisherResult<BoxedProvider> {
        let prov = try!(testing::TestingProvider::new(config));
        Ok(Box::new(prov) as BoxedProvider)
    }

    factory
}


#[macro_export]
macro_rules! create_hook {
    ($tempdir:expr, $name:expr, $( $line:expr ),* ) => {{
        use std::fs;
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;

        let mut hook_path = $tempdir.clone();
        hook_path.push($name);

        let mut hook = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .mode(0o755)
            .open(&hook_path)
            .unwrap();

        let res = write!(hook, "{}", concat!(
            $(
                $line, "\n",
            )*
        ));
        res.unwrap();
    }};
}


pub fn sample_hooks() -> PathBuf {
    // Create a sample directory with some hooks
    let tempdir = utils::create_temp_dir().unwrap();

    create_hook!(tempdir, "example.sh",
        r#"#!/bin/bash"#,
        r#"## Fisher-Testing: {}"#,
        r#"echo "Hello world""#
    );

    create_hook!(tempdir, "failing.sh",
        r#"#!/bin/bash"#,
        r#"## Fisher-Testing: {}"#,
        r#"exit 1"#
    );

    create_hook!(tempdir, "jobs-details.sh",
        r#"#!/bin/bash"#,
        r#"## Fisher-Testing: {}"#,
        r#"b="${FISHER_TESTING_ENV}""#,
        r#"echo "executed" > "${b}/executed""#,
        r#"env > "${b}/env""#,
        r#"pwd > "${b}/pwd""#,
        r#"cat "${FISHER_REQUEST_BODY}" > "${b}/request_body""#,
        r#"cat "prepared" > "${b}/prepared""#
    );

    create_hook!(tempdir, "trigger-status.sh",
        r#"#!/bin/bash"#,
        r#"## Fisher-Testing: {}"#,
        r#"echo "triggering...";"#
    );

    create_hook!(tempdir, "status-example.sh",
        r#"#!/bin/bash"#,
        concat!(
            r#"## Fisher-Status: {"events": ["job_completed", "job_failed"], "#,
            r#""hooks": ["trigger-status"]}"#,
        ),
        r#"echo "triggered!""#
    );

    tempdir
}


enum FakeProcessorInput {
    Recv(mpsc::Sender<ProcessorInput>),
    TryRecv(mpsc::Sender<Option<ProcessorInput>>),
    Stop,
}


pub struct WebAppInstance {
    inst: WebApp,

    url: String,
    client: hyper::Client,
    input_request: mpsc::Sender<FakeProcessorInput>,
}

impl WebAppInstance {

    pub fn new(hooks: Arc<Hooks>, health: bool, behind_proxies: Option<u8>)
               -> Self {
        // Create a new instance of WebApp
        let mut inst = WebApp::new();

        // Create the input channel for the fake processor
        let (input_send, input_recv) = mpsc::channel();
        let (input_request_send, input_request_recv) = mpsc::channel();

        // Create a fake processor
        ::std::thread::spawn(move || {
            for req in input_request_recv.iter() {
                match req {
                    FakeProcessorInput::Recv(return_to) => {
                        return_to.send(
                            input_recv.recv().unwrap()
                        ).unwrap();
                    },
                    FakeProcessorInput::TryRecv(return_to) => {
                        return_to.send(match input_recv.try_recv() {
                            Ok(data) => Some(data),
                            Err(..) => None,
                        }).unwrap();
                    },
                    FakeProcessorInput::Stop => break,
                }
            }
        });

        // Set the options
        let options = FisherOptions {
            bind: "127.0.0.1:0".to_string(),
            enable_health: health,
            behind_proxies: behind_proxies,

            .. FisherOptions::defaults()
        };

        // Start the web server
        let addr = inst.listen(hooks, &options, input_send).unwrap();

        // Create the HTTP client
        let url = format!("http://{}", addr);
        let client = hyper::Client::new();

        WebAppInstance {
            inst: inst,

            url: url,
            client: client,
            input_request: input_request_send,
        }
    }

    pub fn request(&mut self, method: Method, url: &str)
                   -> hyper::RequestBuilder {
        // Create the HTTP request
        self.client.request(method, &format!("{}{}", self.url, url))
    }

    pub fn processor_input(&self) -> Option<ProcessorInput> {
        let (resp_send, resp_recv) = mpsc::channel();

        // Request to the fake processor if there are inputs
        self.input_request.send(
            FakeProcessorInput::TryRecv(resp_send)
        ).unwrap();
        resp_recv.recv().unwrap()
    }

    pub fn next_health(&self, details: HealthDetails) -> NextHealthCheck {
        let input_request = self.input_request.clone();
        let (result_send, result_recv) = mpsc::channel();

        ::std::thread::spawn(move || {
            let (resp_send, resp_recv) = mpsc::channel();

            // Request to the fake processor the next input
            input_request.send(
                FakeProcessorInput::Recv(resp_send)
            ).unwrap();
            let input = resp_recv.recv().unwrap();

            if let ProcessorInput::HealthStatus(ref sender) = input {
                // Send the HealthDetails we want
                sender.send(details).unwrap();

                // Everything was OK
                result_send.send(None).unwrap();
            } else {
                result_send.send(Some(
                    "Wrong kind of ProcessorInput received!".to_string()
                )).unwrap();
            }
        });

        NextHealthCheck::new(result_recv)
    }

    pub fn stop(&mut self) -> bool {
        self.input_request.send(FakeProcessorInput::Stop).unwrap();
        self.inst.stop()
    }
}


pub struct TestingEnv {
    hooks: Arc<Hooks>,
    remove_dirs: Vec<String>,
}

impl TestingEnv {

    pub fn new() -> Self {
        let hooks_dir = sample_hooks().to_str().unwrap().to_string();

        let mut hooks = Hooks::new();
        let mut collected = hooks::collect(&hooks_dir).unwrap();
        for name in collected.keys().cloned().collect::<Vec<String>>() {
            hooks.insert(name.clone(), collected.remove(&name).unwrap());
        }

        TestingEnv {
            hooks: Arc::new(hooks),
            remove_dirs: vec![hooks_dir],
        }
    }

    // CLEANUP

    pub fn delete_also(&mut self, path: &str) {
        self.remove_dirs.push(path.to_string());
    }

    pub fn cleanup(&self) {
        // Remove all the directories
        for dir in &self.remove_dirs {
            let _ = fs::remove_dir_all(dir);
        }
    }

    // JOBS UTILITIES

    pub fn create_job(&self, hook_name: &str, req: Request) -> Job {
        let hook = self.hooks.get(&hook_name.to_string()).unwrap();
        let (_, provider) = hook.validate(&req);

        Job::new(hook.clone(), provider, req)
    }

    // WEB TESTING

    pub fn start_web(&self, health: bool, behind_proxies: Option<u8>)
                     -> WebAppInstance {
        WebAppInstance::new(self.hooks.clone(), health, behind_proxies)
    }
}


pub struct NextHealthCheck {
    result_recv: mpsc::Receiver<Option<String>>,
}

impl NextHealthCheck {

    fn new(result_recv: mpsc::Receiver<Option<String>>) -> Self {
        NextHealthCheck {
            result_recv: result_recv,
        }
    }

    pub fn check(&self) {
        let timeout = Duration::from_secs(5);
        match compat::recv_timeout(&self.result_recv, timeout) {
            Ok(result) => {
                // Propagate panics
                if let Some(message) = result {
                    panic!(message);
                }
            },
            Err(..) => panic!("No ProcessorInput received!"),
        };
    }
}
