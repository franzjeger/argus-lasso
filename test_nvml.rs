use nvml_wrapper::Nvml;
fn main() {
    let nvml = Nvml::init().unwrap();
    let dev = nvml.device_by_index(0).unwrap();
    println!("Util: {:?}", dev.utilization_rates().unwrap());
}
