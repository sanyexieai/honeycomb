use super::*;

#[test]
fn managed_prompt_asset_filter_matches_title_tags_and_summary() {
    let asset = MemoryRoomAsset::new(
        "prompt.extract.semantic-tags",
        "room.project.prompt-library",
        "semantic-tags.md",
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "Semantic Tag Suggester",
        "Infer semantic tags for reviewer and rg.",
    )
    .with_tag("managed_prompt")
    .with_tag("extract");

    assert!(managed_prompt_asset_matches_filter(&asset, "semantic"));
    assert!(managed_prompt_asset_matches_filter(&asset, "reviewer"));
    assert!(managed_prompt_asset_matches_filter(&asset, "extract"));
    assert!(!managed_prompt_asset_matches_filter(&asset, "wenyan"));
}
