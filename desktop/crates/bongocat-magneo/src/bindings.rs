//! The input bindings one model derives from the key artwork it ships.
//!
//! A copy of `bongocat-app/src/model_input.rs`. That module is `pub(crate)`, and
//! the headless publisher has to react to exactly the keys the product does —
//! including dropping a press whose artwork the model never draws — so the
//! mapping is repeated verbatim instead of drifting into a second, simpler rule.

use bongocat_input::{GamepadButton, HandSide, InputBindings, PhysicalKey};
use bongocat_live2d_render::KeyImageInventory;
use bongocat_model::{CommittedModel, ModelOrigin};
use bongocat_render::{FUNCTION_KEY_USAGES, GLOBE_KEY_USAGE, KeyIdentity, KeySide};
use std::collections::BTreeMap;

const RIGHT_ARROW: PhysicalKey = PhysicalKey::from_hid_usage(0x4f);
const RIGHT_ARROW_LAST: u16 = 0x52;

pub fn for_committed_model(model: &CommittedModel) -> InputBindings {
    for_model(
        model.origin(),
        model.id().as_str(),
        &KeyImageInventory::read(model.root()),
    )
}

pub fn for_model(
    origin: ModelOrigin,
    model_id: &str,
    key_images: &KeyImageInventory,
) -> InputBindings {
    let keyboard_model =
        origin == ModelOrigin::Installed || matches!(model_id, "standard" | "keyboard");
    let mut key_hands = BTreeMap::new();

    if keyboard_model {
        for usage in 0x04..=0x65 {
            if (RIGHT_ARROW.hid_usage()..=RIGHT_ARROW_LAST).contains(&usage) {
                continue;
            }
            bind_drawable_key(&mut key_hands, usage, HandSide::Left, key_images);
        }
        for (first, last) in FUNCTION_KEY_USAGES {
            for usage in first..=last {
                bind_drawable_key(&mut key_hands, usage, HandSide::Left, key_images);
            }
        }
        bind_drawable_key(&mut key_hands, 0x67, HandSide::Left, key_images);
        for usage in 0xe0..=0xe7 {
            bind_drawable_key(&mut key_hands, usage, HandSide::Left, key_images);
        }
        bind_drawable_key(&mut key_hands, GLOBE_KEY_USAGE, HandSide::Left, key_images);
    } else {
        bind_drawable_key(
            &mut key_hands,
            PhysicalKey::KEY_A.hid_usage(),
            HandSide::Left,
            key_images,
        );
    }

    if origin == ModelOrigin::Installed || matches!(model_id, "keyboard" | "gamepad") {
        for usage in RIGHT_ARROW.hid_usage()..=RIGHT_ARROW_LAST {
            bind_drawable_key(&mut key_hands, usage, HandSide::Right, key_images);
        }
    }

    InputBindings::with_gamepad_hands(key_hands, gamepad_hands(key_images))
}

fn gamepad_hands(key_images: &KeyImageInventory) -> BTreeMap<GamepadButton, HandSide> {
    let mut hands = BTreeMap::new();
    for button in GamepadButton::ALL {
        let drawable = [HandSide::Left, HandSide::Right]
            .into_iter()
            .find(|side| key_images.can_draw_key(key_side(*side), KeyIdentity::Gamepad(button)));
        if let Some(side) = drawable {
            hands.insert(button, side);
        }
    }
    hands
}

fn bind_drawable_key(
    key_hands: &mut BTreeMap<PhysicalKey, HandSide>,
    hid_usage: u16,
    side: HandSide,
    key_images: &KeyImageInventory,
) {
    if key_images.can_draw(key_side(side), hid_usage) {
        key_hands.insert(PhysicalKey::from_hid_usage(hid_usage), side);
    }
}

const fn key_side(side: HandSide) -> KeySide {
    match side {
        HandSide::Left => KeySide::Left,
        HandSide::Right => KeySide::Right,
    }
}
