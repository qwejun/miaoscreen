//! Where a model sits on the Model library page, and which input mode it belongs
//! to.
//!
//! The build ships one preset per input mode and the user imports their own, so
//! both facts are decided here from the same mode list the import path uses: a
//! mode added upstream reaches the page order, the badges and the import dialog
//! together.

use bongocat_config::{ImportedModelMetadata, ModelInputMode};
use bongocat_model_store::{ModelStoreInputMode, MverInputMode};

/// The preset model that is always available as the final startup fallback.
pub(crate) const STANDARD_PRESET_MODEL_ID: &str = "standard";

pub(crate) const fn model_input_mode_from_mver(mode: MverInputMode) -> ModelInputMode {
    match mode {
        MverInputMode::Standard => ModelInputMode::Standard,
        MverInputMode::Keyboard => ModelInputMode::Keyboard,
        MverInputMode::Gamepad => ModelInputMode::Gamepad,
    }
}

pub(crate) const fn model_input_mode_from_store(mode: ModelStoreInputMode) -> ModelInputMode {
    match mode {
        ModelStoreInputMode::Standard => ModelInputMode::Standard,
        ModelStoreInputMode::Keyboard => ModelInputMode::Keyboard,
        ModelStoreInputMode::Gamepad => ModelInputMode::Gamepad,
    }
}

/// One model the build ships, as the catalog needs to know it.
///
/// Three facts travel together and all three come from the directory the skin
/// was built into: the id is the catalogue key, the title is the name the skin
/// was published under, and the mode is which input device the skin animates.
/// Keeping them in one row means a new skin is one line rather than three
/// tables that can disagree.
struct PresetModel {
    id: &'static str,
    title: &'static str,
    mode: ModelInputMode,
    /// Whether this is one of the product's own three models rather than a
    /// third-party skin.
    ///
    /// The page puts the product's three at the very front and the skins after
    /// them, because the three are what a user sees on first launch and what
    /// the startup fallback resolves to. Without the flag the skins sort
    /// alongside them — every skin animates one of the same three modes, so
    /// ordering by mode alone interleaves the shelves.
    built_in: bool,
}

/// The presets this build ships: the three input modes of the product's own cat,
/// then every third-party skin from the Awesome-BongoCat collection.
///
/// The ids have to be portable ASCII store keys, which is why a skin's name is
/// not its directory name; the Chinese title is what the page shows. Sorted by
/// id so a reader can find a row by eye — lookup is a scan because the list is
/// short and a second sorted index would be a second thing to keep in order.
const PRESET_MODELS: &[PresetModel] = &[
    PresetModel { id: "asetherya", title: "艾瑟雅", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "bangboo", title: "邦布", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "big-silly-tsubaki", title: "大傻椿", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "chisaki", title: "千咲", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "chouyuan", title: "仇远", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "classic-mini-keyboard-keyboard", title: "经典小键盘 · 键盘", mode: ModelInputMode::Keyboard, built_in: false },
    PresetModel { id: "classic-mini-keyboard-standard", title: "经典小键盘", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "closure", title: "归终", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "colleta", title: "珂莱塔", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "crayon-shin-chan-keyboard", title: "蜡笔小新 · 键盘", mode: ModelInputMode::Keyboard, built_in: false },
    PresetModel { id: "da-yuexia", title: "大月下", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "deer-head", title: "鹿头", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "dinosaur-firefly", title: "恐龙流萤", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "doro", title: "doro", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "dream-witch", title: "梦之女巫", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "ebizuka-tomo", title: "海老塚智", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "elysia", title: "爱莉希雅", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "firefly", title: "流萤", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "fool-s-gold", title: "愚人金", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "free-wanderer-keyboard", title: "逍遥散人 · 键盘", mode: ModelInputMode::Keyboard, built_in: false },
    PresetModel { id: "free-wanderer-standard", title: "逍遥散人", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "fuluoluo", title: "弗洛洛", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "gamepad", title: "手柄模式", mode: ModelInputMode::Gamepad, built_in: true },
    PresetModel { id: "gardener", title: "园丁", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "iseri-nina", title: "井芹仁菜", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "jing-liu-gamepad", title: "镜流 · 手柄", mode: ModelInputMode::Gamepad, built_in: false },
    PresetModel { id: "jing-liu-standard", title: "镜流", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "kaedehara-kazuha", title: "枫原万叶", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "kati", title: "卡提", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "kawaragi-momoka", title: "河原木桃香", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "keyboard", title: "键盘模式", mode: ModelInputMode::Keyboard, built_in: true },
    PresetModel { id: "klee", title: "可莉", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "little-girl", title: "小女孩", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "luo-xiaohei", title: "罗小黑", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "mansui", title: "满穗", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "miyabi", title: "星见雅", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "moning", title: "莫宁", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "nalin-keyboard", title: "乃琳 · 键盘", mode: ModelInputMode::Keyboard, built_in: false },
    PresetModel { id: "nalin-standard", title: "乃琳", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "nightwatcher", title: "守夜人", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "ogiyama-mahiro", title: "绪山真寻", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "phoebe", title: "菲比", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "pidan", title: "皮蛋", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "pink-angel", title: "粉红天使", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "power-kitty", title: "动力小猫", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "red-panda-haohao-keyboard", title: "小熊猫昊昊 · 键盘", mode: ModelInputMode::Keyboard, built_in: false },
    PresetModel { id: "red-panda-haohao-standard", title: "小熊猫昊昊", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "ruan-mei-gamepad", title: "阮梅 · 手柄", mode: ModelInputMode::Gamepad, built_in: false },
    PresetModel { id: "ruan-mei-keyboard", title: "阮梅 · 键盘", mode: ModelInputMode::Keyboard, built_in: false },
    PresetModel { id: "ruan-mei-standard", title: "阮梅", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "rupa", title: "RUPA", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "sangonomiya-kokomi", title: "珊瑚宫心海", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "shore-keeper", title: "守岸人", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "silver-wolf", title: "银狼", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "sinister-lizard", title: "孽蜥", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "standard", title: "标准模式", mode: ModelInputMode::Standard, built_in: true },
    PresetModel { id: "suxin", title: "塑心", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "the-king-in-yellow", title: "黄衣之主", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "undertaker", title: "送葬人", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "venti", title: "温迪", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "wendy", title: "温蒂", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "xiao-gen-2", title: "魈二代", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "yana-subaru", title: "安和昴", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "yumi", title: "悠米", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "zimin", title: "籽岷", mode: ModelInputMode::Standard, built_in: false },
    PresetModel { id: "ziyin", title: "子音", mode: ModelInputMode::Standard, built_in: false },
];

fn preset_model(id: &str) -> Option<&'static PresetModel> {
    PRESET_MODELS.iter().find(|model| model.id == id)
}

/// The display name of a preset the build ships, or `None` for an id the build
/// does not know — a package a developer dropped into the catalog root.
pub(crate) fn preset_model_title(id: &str) -> Option<&'static str> {
    preset_model(id).map(|model| model.title)
}

/// The input mode a preset animates, or `None` for an id outside this list.
///
/// The Model library page reads this for the badge on the card, and the import
/// path reads it to decide which mode a preset belongs to.
pub(crate) fn preset_model_input_mode(id: &str) -> Option<ModelInputMode> {
    preset_model(id).map(|model| model.mode)
}

/// Where a preset model sits on the Model library page: the product's own three
/// modes first, then the third-party skins grouped by the mode they animate.
///
/// The modes declare their own order — [`MverInputMode::ALL`] is Standard,
/// Keyboard, Gamepad, and that order is part of the settings contract because
/// it is the order a conversion reports and titles its models in. The rank is
/// read from that list rather than repeated here, so the two cannot drift.
///
/// The two shelves are separate because a skin animates one of the same three
/// modes: ranking purely by mode would interleave 艾瑟雅 between `standard` and
/// `keyboard`, and the page would open on a stranger's cat instead of the one
/// the product ships. The product's three therefore hold ranks `0..3` and the
/// skins `3..6`, in mode order inside each shelf.
///
/// A preset not in the table — a package a developer dropped into the catalog
/// root — sorts after both shelves.
pub(crate) fn preset_model_order(id: &str) -> usize {
    let shelf = MverInputMode::ALL.len();
    match preset_model(id) {
        Some(model) => {
            let rank = MverInputMode::ALL
                .iter()
                .position(|candidate| {
                    Some(model_input_mode_from_mver(*candidate)) == Some(model.mode)
                })
                .unwrap_or(shelf);
            if model.built_in { rank } else { shelf + rank }
        }
        None => shelf * 2,
    }
}

/// Where an installed model sits on the Model library page: the order the user
/// imported it in.
///
/// The configuration's record list is that order. An import appends its record
/// and a deletion only removes one, so a record's position is the model's place
/// on the page and a newly imported model always lands at the end.
///
/// An entry with no record — a package directory copied into the store root by
/// hand, which no import ever ran for — has no place in that order, so it sorts
/// after every model the user actually imported. Ordering those by id keeps the
/// page stable rather than dependent on the order the scan happened to walk the
/// directory in.
pub(crate) fn installed_model_order(records: &[ImportedModelMetadata], id: &str) -> usize {
    records
        .iter()
        .position(|record| record.id == id)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bongocat_model::ModelId;
    use std::collections::BTreeSet;

    /// The table is hand-generated from the collection's own file names, so the
    /// thing worth pinning is that it is still a table the catalogue can use:
    /// every id a portable store key, no id twice, every row named, and exactly
    /// the three built-ins the product ships itself.
    #[test]
    fn the_preset_table_names_every_skin_once() {
        let mut ids = BTreeSet::new();
        for model in PRESET_MODELS {
            assert!(
                ModelId::parse(model.id).is_ok(),
                "{:?} is not a portable model id",
                model.id
            );
            assert!(
                !model.title.trim().is_empty(),
                "{:?} has no display title",
                model.id
            );
            assert!(ids.insert(model.id), "{:?} appears twice", model.id);
        }
        assert_eq!(
            PRESET_MODELS.iter().filter(|model| model.built_in).count(),
            3,
            "the product ships three models of its own"
        );
    }

    /// The page opens on the product's own cat: the three modes hold the first
    /// ranks, every skin sorts after them, and an id the build does not know
    /// sorts last.
    #[test]
    fn the_three_modes_hold_the_front_ranks() {
        assert_eq!(preset_model_order("standard"), 0);
        assert_eq!(preset_model_order("keyboard"), 1);
        assert_eq!(preset_model_order("gamepad"), 2);
        for model in PRESET_MODELS.iter().filter(|model| !model.built_in) {
            assert!(
                preset_model_order(model.id) >= 3,
                "{:?} sorts among the product's own models",
                model.id
            );
        }
        assert_eq!(preset_model_order("not-a-preset"), 6);
    }

    #[test]
    fn every_built_in_answers_with_its_mode_and_title() {
        assert_eq!(preset_model_input_mode("standard"), Some(ModelInputMode::Standard));
        assert_eq!(preset_model_input_mode("keyboard"), Some(ModelInputMode::Keyboard));
        assert_eq!(preset_model_input_mode("gamepad"), Some(ModelInputMode::Gamepad));
        assert_eq!(preset_model_title("standard"), Some("标准模式"));
        assert_eq!(preset_model_title("not-a-preset"), None);
    }
}
