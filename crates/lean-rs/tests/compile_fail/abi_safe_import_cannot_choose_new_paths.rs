use lean_rs::host_bridge::HostShimBindings;

fn import_unattested(bindings: &HostShimBindings<'_, '_>) {
    let _ = bindings.session_import(vec!["new-path".to_owned()], vec!["NewModule".to_owned()]);
}

fn main() {}
