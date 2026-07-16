use super::{
    AnnotationQuery, IndexedAnnotationWriter, LiveStoreConfig, StoreStatus, WordPresenceBucket,
};
use crate::events::{Annotation, Word};

fn config() -> LiveStoreConfig {
    LiveStoreConfig {
        hot_tail_publish_words: 1,
        ..LiveStoreConfig::default()
    }
}

#[test]
fn backend_contract_append_query_finish() {
    let (mut writer, store) = IndexedAnnotationWriter::create(config()).unwrap();
    writer
        .append_batch(&[Word::spanning(0x11, 100, 20), Word::new(0x22, 200)])
        .unwrap();

    let exact = store.exact_window(0, 300, 10).unwrap();
    assert!(exact.complete);
    assert_eq!(
        exact.annotations,
        vec![
            Annotation {
                start_ns: 100,
                end_ns: 120,
                value: 0x11,
            },
            Annotation {
                start_ns: 200,
                end_ns: 200,
                value: 0x22,
            },
        ]
    );

    let presence = store.presence_window(0, 300, 16).unwrap();
    assert!(presence.iter().map(|bucket| bucket.word_count).sum::<u64>() >= 2);
    assert!(presence.iter().all(|bucket: &WordPresenceBucket| {
        bucket.start_ns <= bucket.end_ns && bucket.word_count > 0
    }));
    assert_eq!(store.nearest_boundary(118, 5).unwrap(), Some(120));

    writer.finish().unwrap();
    assert_eq!(store.snapshot().metadata.status, StoreStatus::Finished);
    assert_eq!(store.metadata().total_word_count, 2);
}

#[test]
fn backend_contract_cancel() {
    let (mut writer, store) = IndexedAnnotationWriter::create(config()).unwrap();
    writer.append(Word::new(0x33, 100)).unwrap();
    writer.cancel().unwrap();

    assert_eq!(store.snapshot().metadata.status, StoreStatus::Cancelled);
}

#[test]
fn backend_contract_rejects_out_of_order_words() {
    let (mut writer, _store) = IndexedAnnotationWriter::create(config()).unwrap();
    let error = writer
        .append_batch(&[Word::new(1, 200), Word::new(2, 100)])
        .unwrap_err();
    assert!(error.to_string().contains("backwards"));
}

#[test]
fn backend_contract_sparse_instantaneous_words_leave_gaps_empty() {
    let (mut writer, store) = IndexedAnnotationWriter::create(config()).unwrap();
    writer
        .append_batch(&[
            Word::new(1, 1_000),
            Word::new(2, 1_100),
            Word::new(3, 10_001_100),
        ])
        .unwrap();

    let exact = store.exact_window(0, 20_000_000, 10).unwrap();
    assert_eq!(exact.annotations[0].end_ns, 1_100);
    assert_eq!(exact.annotations[1].end_ns, 1_200);
    assert!(exact.annotations[1].end_ns < exact.annotations[2].start_ns);
    assert_eq!(store.nearest_boundary(1_199, 2).unwrap(), Some(1_200));
}
