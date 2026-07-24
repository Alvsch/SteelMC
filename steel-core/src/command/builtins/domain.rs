//! Steel domain switch command.

use std::sync::Arc;

use steel_registry::{
    data_components::vanilla_components::{CUSTOM_NAME, ENCHANTMENT_GLINT_OVERRIDE},
    vanilla_dimension_types, vanilla_items, vanilla_menu_types,
};
use steel_utils::Identifier;
use text_components::TextComponent;

use crate::{inventory::prelude::*, portal::WorldChangeRequest, server::Server, world::World};

use super::super::{
    brigadier::{CommandNodeBuilder, CommandSyntaxError},
    execution::{
        CommandSource, SteelArgumentType, SteelCommandContext, SteelCommandRuntime, argument,
        literal,
    },
    registration::CommandRegistration,
};

pub(super) fn registration() -> CommandRegistration<CommandSource> {
    CommandRegistration::new(Identifier::from_steel("domain"), |_| command())
}

fn command() -> CommandNodeBuilder<CommandSource, SteelCommandRuntime> {
    literal("domain")
        .executes(|ctx: &SteelCommandContext<CommandSource>| {
            let Some(player) = ctx.source().player() else {
                return Err(CommandSyntaxError::dynamic(
                    "you cannot use this command from the console",
                ));
            };

            player.open_menu("Domains", |container_id, world| {
                domain_menu(container_id, player.clone(), world, ctx.source())
            });

            Ok(1)
        })
        .then(argument("domain", SteelArgumentType::domain()).executes(switch_domain))
}

fn switch_domain(context: &SteelCommandContext<CommandSource>) -> Result<i32, CommandSyntaxError> {
    let source = context.source();
    let Some(player) = source.player() else {
        return Err(CommandSyntaxError::dynamic(
            "This command can only be used by a player",
        ));
    };
    let Some(domain) = context.domain("domain") else {
        return Err(CommandSyntaxError::dynamic(
            "Parsed domain is missing from the command context",
        ));
    };
    source
        .server()
        .queue_domain_switch(Arc::clone(player), domain.to_owned())
        .map_err(CommandSyntaxError::dynamic)?;

    source.send_success(
        &TextComponent::plain(format!("Switching to domain {domain}")),
        true,
    );
    Ok(1)
}

fn domain_menu(
    container_id: u8,
    player: Arc<Player>,
    current_world: &Arc<World>,
    source: &CommandSource,
) -> Menu {
    let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X6, container_id);

    let server = source.server();

    let domain_names: Vec<String> = server
        .worlds
        .domain_names()
        .map(ToOwned::to_owned)
        .collect();

    let map: Vec<(Section, Vec<Arc<World>>)> = b.grid(6, |g| {
        g.paint_all(ItemStack::empty());
        g.paint(
            Rect::cols(..).rows(0),
            ItemStack::new(&vanilla_items::GRAY_STAINED_GLASS_PANE),
        );
        g.paint(
            Rect::cols(..).rows(5),
            ItemStack::new(&vanilla_items::GRAY_STAINED_GLASS_PANE),
        );
        g.paint(
            Rect::cols(8).rows(..),
            ItemStack::new(&vanilla_items::GRAY_STAINED_GLASS_PANE),
        );
        g.paint(
            Rect::cols(0).rows(1..5),
            ItemStack::new(&vanilla_items::GRAY_STAINED_GLASS_PANE),
        );

        domain_names
            .iter()
            .take(4)
            .enumerate()
            .map(|(i, domain_name)| {
                g.subgrid(Rect::cols(..8).rows(i + 1), |g| {
                    g.paint_all(ItemStack::empty());

                    let mut sign = ItemStack::new(&vanilla_items::OAK_SIGN);
                    sign.set(CUSTOM_NAME, domain_name.clone().into());

                    g.paint(Rect::cell(0, 0), sign);

                    let worlds = server.worlds.worlds_in_domain(domain_name);

                    let icons: Vec<ItemStack> =
                        worlds.iter().map(|w| icon(w, current_world)).collect();

                    let len = icons.len();

                    let container = SimpleContainer::from_items(icons).into_shared();

                    (
                        g.place(Rect::cols(1..(len + 1).min(6)).rows(0), container)
                            .display()
                            .region()
                            .single(),
                        worlds,
                    )
                })
            })
            .collect()
    });
    b.player_inventory(&player.inventory);

    b.build(MenuKindType::custom(DomainMenuKind {
        map,
        server: server.clone(),
        player,
    }))
}

fn icon(world: &Arc<World>, current_world: &Arc<World>) -> ItemStack {
    let item = match world.dimension_type {
        b if b == &vanilla_dimension_types::OVERWORLD
            || b == &vanilla_dimension_types::OVERWORLD_CAVES =>
        {
            &vanilla_items::GRASS_BLOCK
        }
        b if b == &vanilla_dimension_types::THE_NETHER => &vanilla_items::NETHERRACK,
        b if b == &vanilla_dimension_types::THE_END => &vanilla_items::END_STONE,
        _ => &vanilla_items::BEDROCK,
    };
    let mut icon = ItemStack::new(item);
    icon.set(CUSTOM_NAME, world.key.path.to_string().into());
    if world.key == current_world.key {
        icon.set(ENCHANTMENT_GLINT_OVERRIDE, true);
    }
    icon
}

struct DomainMenuKind {
    map: Vec<(Section, Vec<Arc<World>>)>,
    server: Arc<Server>,
    player: Arc<Player>,
}

impl MenuKind for DomainMenuKind {
    fn on_slot_clicked(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        click: Click,
        _player: &Player,
    ) -> ClickOutcome {
        let Some(index) = click.slot() else {
            return ClickOutcome::Fallthrough;
        };

        let Some((section, worlds)) = self
            .map
            .iter()
            .find(|(section, _worlds)| section.contains(index))
        else {
            return ClickOutcome::Fallthrough;
        };

        if worlds.is_empty() {
            return ClickOutcome::Consume;
        }

        let world = &worlds[index - section.start()];

        if world.domain() == self.player.get_world().domain() {
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

#[cfg(test)]
mod tests {
    use super::super::create_dispatcher;
    use crate::command::{
        brigadier::{CommandDispatcher, NodeId},
        execution::{CommandSource, SteelArgumentType, SteelCommandRuntime},
    };
    use steel_registry::test_support::init_test_registry;

    type Dispatcher = CommandDispatcher<CommandSource, SteelCommandRuntime>;

    fn child(dispatcher: &Dispatcher, parent: NodeId, name: &str) -> NodeId {
        let Some(children) = dispatcher.children(parent) else {
            panic!("parent node should exist");
        };
        let Some(child) = children.iter().copied().find(|child| {
            dispatcher
                .node(*child)
                .is_some_and(|node| node.name() == name)
        }) else {
            panic!("child {name} should exist");
        };
        child
    }

    #[test]
    fn domain_graph_uses_the_configured_domain_argument() {
        init_test_registry();
        let Ok(dispatcher) = create_dispatcher() else {
            panic!("built-in commands should register");
        };
        let root = child(&dispatcher, dispatcher.root(), "domain");
        let domain = child(&dispatcher, root, "domain");
        assert_eq!(
            dispatcher
                .node(domain)
                .and_then(|node| node.argument_type()),
            Some(&SteelArgumentType::domain())
        );
        let Some(domain) = dispatcher.node(domain) else {
            panic!("domain argument should exist");
        };
        assert!(domain.is_executable());
    }
}
