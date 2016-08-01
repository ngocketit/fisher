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

extern crate rustc_serialize;
extern crate regex;
extern crate ansi_term;
extern crate chan_signal;
extern crate hyper;
#[macro_use] extern crate chan;
#[macro_use] extern crate nickel;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate clap;

mod cli;
mod providers;
mod hooks;
mod errors;
mod processor;
mod web;

use ansi_term::Colour;
use chan_signal::Signal;


fn get_hooks(base: &String) -> hooks::Hooks {
    // Actually collect hooks
    let collected_hooks = hooks::collect(base);

    // Show an error and exit if something went wrong
    if collected_hooks.is_err() {
        println!("{} {}",
                 Colour::Red.bold().paint("Error:"),
                 collected_hooks.err().unwrap());
        ::std::process::exit(1);
    }

    let hooks = collected_hooks.unwrap();
    println!("Total hooks collected: {}", hooks.len());

    hooks
}


fn main() {
    let exit_signal = chan_signal::notify(&[Signal::INT, Signal::TERM]);

    let options = cli::parse();
    let hooks = get_hooks(&options.hooks_dir);

    let mut processor = processor::ProcessorInstance::new(&hooks);
    let mut webapi = web::WebAPI::new();

    // Start the web application
    webapi.listen(&options.bind);

    // Wait until SIGINT or SIGTERM is received
    exit_signal.recv().unwrap();

    // Let the web application close itself
    webapi.stop();
}
