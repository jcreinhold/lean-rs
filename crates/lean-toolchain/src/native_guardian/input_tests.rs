use super::*;

fn input_record(fixture: &Fixture) -> (PathBuf, PathBuf, Child) {
    let record = reserve(fixture, 1, &slot("utility", "preparation", 1)).unwrap();
    let inputs = create_generation_inputs(record.parent().unwrap()).unwrap();
    let child = private_group();
    running(&record, &child);
    (record, inputs, child)
}
#[test]
fn live_payload_keeps_inputs_until_closed_record_collection() {
    let fixture = Fixture::new();
    let (record, inputs, mut child) = input_record(&fixture);
    fs::write(inputs.join("source.lean"), b"def live := 1").unwrap();
    assert_eq!(
        reserve(&fixture, 2, &slot("next", "preparation", 1))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(inputs.join("source.lean").exists());
    assert_eq!(read_owner(&record).unwrap()["phase"], "running");
    stopped(&mut child);
    reserve(&fixture, 3, &slot("next", "preparation", 1)).unwrap();
    assert!(!inputs.exists());
    assert!(!record.exists());
}
#[test]
fn nested_and_root_symlinks_are_unlinked_without_following_external_sentinels() {
    for root_alias in [false, true] {
        let fixture = Fixture::new();
        let (record, inputs, mut child) = input_record(&fixture);
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"untouched").unwrap();
        if root_alias {
            fs::remove_dir(&inputs).unwrap();
            std::os::unix::fs::symlink(&outside, &inputs).unwrap();
            assert!(generation_inputs(record.parent().unwrap()).is_err());
        } else {
            fs::create_dir(inputs.join("cache")).unwrap();
            std::os::unix::fs::symlink(&outside, inputs.join("cache").join("external")).unwrap();
            std::os::unix::fs::symlink(outside.join("sentinel"), inputs.join("file-alias")).unwrap();
        }
        stopped(&mut child);
        reserve(&fixture, 2, &slot("next", "preparation", 1)).unwrap();
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"untouched");
        assert!(!record.exists());
        assert!(fs::symlink_metadata(inputs).is_err());
    }
}
#[test]
fn count_bounded_partial_cleanup_preserves_the_committed_owner_for_retry() {
    let fixture = Fixture::new();
    let (record, inputs, mut child) = input_record(&fixture);
    for index in 0..4097 {
        fs::write(inputs.join(format!("{index:04}")), []).unwrap();
    }
    stopped(&mut child);
    assert!(reserve(&fixture, 2, &slot("next", "preparation", 1)).is_err());
    assert_eq!(read_owner(&record).unwrap()["phase"], "running");
    assert!(
        fs::read_dir(&inputs).unwrap().count() < 4097,
        "bounded deletion made progress without erasing ownership"
    );
    reserve(&fixture, 3, &slot("next", "preparation", 1)).unwrap();
    assert!(!record.exists());
    assert!(!inputs.exists());
}
#[test]
fn unknown_generation_children_preserve_record_and_inputs() {
    let fixture = Fixture::new();
    let (record, inputs, mut child) = input_record(&fixture);
    fs::write(inputs.join("owned"), b"input").unwrap();
    let unknown = record.parent().unwrap().join("unrecognized");
    fs::write(&unknown, b"unowned").unwrap();
    stopped(&mut child);
    assert!(reserve(&fixture, 2, &slot("next", "preparation", 1)).is_err());
    assert_eq!(read_owner(&record).unwrap()["phase"], "running");
    assert!(inputs.join("owned").exists());
    assert!(unknown.exists());
    fs::remove_file(unknown).unwrap();
    reserve(&fixture, 3, &slot("next", "preparation", 1)).unwrap();
    assert!(!record.exists());
}
#[test]
fn depth_bounded_cleanup_preserves_owner_and_never_follows_an_alias() {
    let fixture = Fixture::new();
    let (record, inputs, mut child) = input_record(&fixture);
    let mut current = inputs;
    for _ in 0..34 {
        current = current.join("nested");
        fs::create_dir(&current).unwrap();
    }
    fs::write(current.join("source.lean"), b"too deep").unwrap();
    stopped(&mut child);
    assert!(reserve(&fixture, 2, &slot("next", "preparation", 1)).is_err());
    assert!(record.exists());
    assert!(current.join("source.lean").exists());
}

#[test]
fn cleanup_after_the_owner_generation_was_collected_is_idempotent() {
    let fixture = Fixture::new();
    let (record, inputs, mut child) = input_record(&fixture);
    let generation = record.parent().unwrap().to_path_buf();
    fs::write(inputs.join("owned"), b"input").unwrap();
    stopped(&mut child);
    reserve(&fixture, 2, &slot("next", "preparation", 1)).unwrap();
    assert!(!generation.exists());
    cleanup_generation_inputs(&generation).unwrap();
}

#[test]
fn retirement_cannot_close_a_running_payload_or_another_preparing_owner() {
    let fixture = Fixture::new();
    let (record, _inputs, mut child) = input_record(&fixture);
    assert!(retire_preparing_owner_record(&record, &format!("{:048x}", 1)).is_err());
    assert_eq!(read_owner(&record).unwrap()["phase"], "running");
    assert!(retire_preparing_owner_record(&record, &format!("{:048x}", 2)).is_err());
    stopped(&mut child);
    let other = reserve(&fixture, 2, &slot("other", "preparation", 1)).unwrap();
    let mut value = read_owner(&other).unwrap();
    value["parent"]["start"] = json!("a-different-parent-incarnation");
    persist_owner(&other, &value).unwrap();
    assert!(retire_preparing_owner_record(&other, &format!("{:048x}", 2)).is_err());
    assert_eq!(read_owner(&other).unwrap()["phase"], "preparing");
}
