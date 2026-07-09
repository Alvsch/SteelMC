//! Handler for the "domain" command.

use std::borrow::ToOwned;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::command::arguments::domain::DomainArgument;
use crate::command::commands::{CommandHandlerBuilder, CommandHandlerDyn, argument};
use crate::command::context::CommandContext;
use crate::command::error::CommandError;
use crate::inventory::FillDirection::{self};
use crate::inventory::lock::{ContainerLockGuard, ContainerRef};
use crate::inventory::menu::MenuBehavior;
use crate::inventory::simple_menu::SimpleContainer;
use crate::inventory::{Click, ClickOutcome, MenuBuilder, MenuKind, MenuKindType, Section};
use crate::player::Player;
use crate::portal::WorldChangeRequest;
use crate::server::Server;
use crate::world::World;
use steel_registry::data_components::components::ItemLore;
use steel_registry::data_components::vanilla_components::{
    CUSTOM_NAME, ENCHANTMENT_GLINT_OVERRIDE, LORE,
};
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_dimension_types, vanilla_items, vanilla_menu_types};
use steel_utils::locks::SyncMutex;
use text_components::format::Color;
use text_components::{Modifier, TextComponent};

/// Handler for switching to another configured domain.
#[expect(clippy::too_many_lines, reason = "dont annoy me")]
#[must_use]
pub fn command_handler() -> impl CommandHandlerDyn {
    CommandHandlerBuilder::new(
        &["domain"],
        "Switches to another configured domain.",
        "minecraft:command.domain",
    )
    .executes(|(), ctx: &mut CommandContext| {
        let Some(player) = &ctx.player else {
            return Err(CommandError::CommandFailed(Box::new(
                "You cannot use this command from the console".into(),
            )));
        };

        player.open_menu(
            TextComponent::const_plain("Domains").bold(true),
            |id, current_world| {
                let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X6, id);

                let mut items = vec![ItemStack::empty(); 9 * 6];

                let server = ctx.server.clone();
                let domains: Vec<String> = server
                    .worlds
                    .domain_names()
                    .map(ToOwned::to_owned)
                    .collect();

                #[expect(clippy::needless_range_loop, reason = "dont annoy me")]
                for i in 0..54 {
                    if !(9..45).contains(&i) || i % 9 == 8 {
                        items[i] = ItemStack::new(&vanilla_items::ITEMS.gray_stained_glass_pane);
                    }
                }

                let mut map: BTreeMap<usize, Arc<World>> = BTreeMap::new();

                for (i, name) in domains.into_iter().enumerate() {
                    let worlds = server.worlds.worlds_in_domain(&name);

                    let mut sign = ItemStack::new(&vanilla_items::ITEMS.oak_sign);
                    if current_world.domain() == &*name {
                        sign.set(ENCHANTMENT_GLINT_OVERRIDE, true);
                    }
                    sign.set(CUSTOM_NAME, name.into());
                    let row_start = (i + 1) * 9;
                    items[row_start] = sign;

                    for (j, world) in worlds.iter().enumerate() {
                        let item = match world.dimension_type {
                            b if b == &vanilla_dimension_types::OVERWORLD
                                || b == &vanilla_dimension_types::OVERWORLD_CAVES =>
                            {
                                &vanilla_items::ITEMS.grass_block
                            }
                            b if b == &vanilla_dimension_types::THE_NETHER => {
                                &vanilla_items::ITEMS.netherrack
                            }
                            b if b == &vanilla_dimension_types::THE_END => {
                                &vanilla_items::ITEMS.end_stone
                            }
                            _ => &vanilla_items::ITEMS.bedrock,
                        };
                        let mut icon = ItemStack::new(item);
                        icon.set(
                            CUSTOM_NAME,
                            TextComponent::plain(world.key.to_string()).color(Color::Gray),
                        );
                        let mut lines: Vec<TextComponent> = Vec::new();
                        lines.push(TextComponent::plain(""));
                        world.players.iter_players(|_uuid, p| {
                            lines.push(
                                TextComponent::from(format!("- {}", p.gameprofile.name))
                                    .color(Color::DarkGray),
                            );
                            true
                        });
                        icon.set(LORE, ItemLore::new(lines));
                        if current_world.key == world.key {
                            icon.set(ENCHANTMENT_GLINT_OVERRIDE, true);
                        }
                        items[row_start + j + 1] = icon;
                        map.insert(row_start + j + 1, world.clone());
                    }
                }

                let content = Arc::new(SyncMutex::new(SimpleContainer::from_items(items)));
                let content_ref = ContainerRef::SimpleContainer(content.clone());

                let content_section = b.restricted_section(
                    content_ref,
                    9 * 6,
                    |_| false,
                    Some(|_: &ContainerLockGuard, _: &Player, _: &ItemStack| false),
                );

                let player_inv = b.player_inventory(&player.inventory);

                b.route(content_section, [player_inv.all()], FillDirection::Backward);
                b.route(
                    player_inv.hotbar(),
                    [content_section],
                    FillDirection::Forward,
                );
                b.route(
                    player_inv.main(),
                    [player_inv.hotbar()],
                    FillDirection::Forward,
                );

                b.build(MenuKindType::Custom(Box::new(DomainMenuKind {
                    server: server.clone(),
                    player: player.clone(),
                    restricted_section: content_section,
                    map,
                })))
            },
        );

        Ok(())
    })
    .then(argument("domain", DomainArgument).executes(
        |((), domain): ((), String), context: &mut CommandContext| -> Result<(), CommandError> {
            let player = context
                .sender
                .get_player()
                .cloned()
                .ok_or(CommandError::InvalidRequirement)?;
            let server = context.server.clone();
            server
                .queue_domain_switch(player, domain.clone())
                .map_err(|error| {
                    CommandError::CommandFailed(Box::new(TextComponent::plain(error)))
                })?;

            context.sender.send_message(&TextComponent::plain(format!(
                "Switching to domain {domain}"
            )));
            Ok(())
        },
    ))
}

struct DomainMenuKind {
    server: Arc<Server>,
    player: Arc<Player>,
    restricted_section: Section,
    map: BTreeMap<usize, Arc<World>>,
}

impl MenuKind for DomainMenuKind {
    fn on_slot_clicked(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        click: Click,
        player: &Player,
    ) -> ClickOutcome {
        #[expect(clippy::manual_let_else, reason = "just doesnt look good")]
        let index = match click {
            Click::Pickup { slot, button: _ }
            | Click::QuickMove { slot }
            | Click::Clone { slot } => slot,
            _ => {
                return ClickOutcome::Fallthrough;
            }
        };

        if !self.restricted_section.contains(index) {
            return ClickOutcome::Fallthrough;
        }

        let Some((_, world)) = self.map.get_key_value(&index) else {
            return ClickOutcome::Consume;
        };

        if world.domain() == player.get_world().domain() {
            self.server.queue_world_change(
                self.player.clone(),
                WorldChangeRequest::WorldSpawn {
                    target_world: world.clone(),
                },
            );
        } else {
            match self
                .server
                .queue_domain_switch_to_world(self.player.clone(), world.clone())
            {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!(e);
                }
            }
        }

        ClickOutcome::Consume
    }
}
