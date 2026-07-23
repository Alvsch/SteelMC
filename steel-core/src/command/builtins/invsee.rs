use std::sync::Arc;

use steel_registry::vanilla_menu_types;
use steel_utils::Identifier;
use text_components::TextComponent;

use super::super::{
    brigadier::{CommandNodeBuilder, CommandSyntaxError},
    execution::{CommandSource, SteelArgumentType, SteelCommandRuntime, argument, literal},
    registration::CommandRegistration,
};
use crate::entity::Entity;
use crate::inventory::menu::Menu;
use crate::inventory::prelude::*;
use crate::inventory::slots::{NormalSlot, armor_slots};
use crate::player::Player;

pub(super) fn registration() -> CommandRegistration<CommandSource> {
    CommandRegistration::new(Identifier::vanilla_static("invsee"), |_| command())
}

fn command() -> CommandNodeBuilder<CommandSource, SteelCommandRuntime> {
    literal("invsee").then(
        argument("target", SteelArgumentType::player()).executes(|ctx| {
            let target = ctx.player("target")?;
            let Some(source) = ctx.source().player() else {
                return Err(CommandSyntaxError::dynamic(TextComponent::const_plain(
                    "you cannot use this command from the console",
                )));
            };
            source.open_menu(target.display_name(), |container_id, _world| {
                invsee(container_id, source, &target)
            });
            Ok(0)
        }),
    )
}

fn invsee(container_id: u8, source: &Arc<Player>, target: &Arc<Player>) -> Menu {
    let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X5, container_id);

    let target_ref = ContainerRef::from(target.inventory.clone());
    let target_inventory = b.player_inventory(&target.inventory);

    let (armor, offhand) = b.grid(1, |g| {
        let armor = g.place_slots(
            Rect::cols(0..4).rows(..),
            armor_slots(&target.inventory),
            [target_ref.clone()],
        );
        let offhand = g.place_slots(
            Rect::cols(4).rows(..),
            [SlotType::Normal(NormalSlot::new(target_ref.clone(), 40))],
            [target_ref],
        );
        g.place_display(
            Rect::cols(5..).rows(..),
            ContainerRef::from(target.crafting_container()),
        );
        (armor.single(), offhand.single())
    });

    let viewer = b.player_inventory(&source.inventory);
    b.route(
        target_inventory.all(),
        [viewer.all()],
        FillDirection::Backward,
    );
    b.route([armor, offhand], [viewer.all()], FillDirection::Backward);
    b.route(
        viewer.all(),
        [target_inventory.all()],
        FillDirection::Forward,
    );

    b.build(MenuKindType::Custom(Box::new(InvseeMenuKind {})))
}

#[derive(Debug)]
struct InvseeMenuKind {}

impl MenuKind for InvseeMenuKind {}
