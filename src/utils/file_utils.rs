//! Utility functions for rllvm

use std::{fs, path::Path};

use object::{Object, ObjectKind};

pub fn is_plain_file<P>(file: P) -> bool
where
    P: AsRef<Path>,
{
    let file = file.as_ref();
    if !file.exists() {
        false
    } else if file.is_dir() {
        false
    } else {
        true
    }
}

pub fn is_object_file<P>(file: P) -> bool
where
    P: AsRef<Path>,
{
    let file = file.as_ref();

    if !is_plain_file(file) {
        return false;
    }

    let data = fs::read(file).expect("Failed to read file");
    let obj_file = object::File::parse(&*data).expect("Failed to parse the object file");

    obj_file.kind() == ObjectKind::Relocatable
}
