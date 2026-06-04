use std::path::Path;

pub trait IndexProgress {
    fn directory_started(&mut self, directory: &Path);

    fn status(&mut self, _message: &str) {}
}

#[derive(Debug, Default)]
pub struct NoopProgress;

impl IndexProgress for NoopProgress {
    fn directory_started(&mut self, _directory: &Path) {}
}
