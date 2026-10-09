use api::cli::Cli;
use vmm::vmm::Vmm;

/// The crates log through `log`; without a logger their warnings and errors are lost, and
/// the service's stderr is the kernel log on Android, so that is where they go
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("bao-virtio-dm: {}: {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn main() {
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));

    // Create a new CLI object.
    let cli = Cli::new();

    // Launch the CI to parse the configuration file.
    let vmm_config = cli.launch().unwrap();

    // Create a new VMM.
    let vmm = Vmm::try_from(vmm_config).unwrap();

    // Run the VMM.
    vmm.run().unwrap();
}
