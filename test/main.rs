use std::ffi::CString;

use dpdk::{rte_eal_init, rte_lcore_id_};

fn main() {
    let argc = 1;
    let mut argv = Vec::new();
    argv.push(CString::new("../prefix").unwrap());

    let mut c_argv = Vec::new();
    for arg in argv.iter_mut() {
        c_argv.push(arg.as_ptr() as *mut i8);
    }

    unsafe { rte_eal_init(argc, c_argv.as_mut_ptr()) };

    println!("{}", unsafe { rte_lcore_id_() });
}
