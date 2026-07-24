use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use steel_registry::vanilla_menu_types;
use steel_utils::Identifier;
use text_components::TextComponent;

use super::super::{
    brigadier::{CommandNodeBuilder, CommandSyntaxError},
    execution::{
        CommandPermissionSource, CommandSource, SteelArgumentType, SteelCommandRuntime, argument,
        literal,
    },
    registration::{CommandRegistration, CommandRegistrationError},
};
use crate::entity::Entity;
use crate::inventory::menu::Menu;
use crate::inventory::prelude::*;
use crate::inventory::slots::{
    CraftingHandler, MayPickupFn, MayPlaceFn, NormalSlot, RestrictedSlot,
};
use crate::permission::{PermissionExpr, PermissionKey, PermissionKeyError};
use crate::player::{Player, connection::NetworkConnection};

const INVSEE_PERMISSION: &str = "steel.command.invsee";
const MODIFY_PERMISSION: &str = "steel.command.invsee.modify";

pub(super) fn registration() -> Result<CommandRegistration<CommandSource>, CommandRegistrationError>
{
    let id = Identifier::from_steel("invsee");
    let (access_permission, modify_permission) = invsee_permissions().map_err(|source| {
        CommandRegistrationError::InvalidExplicitPermission {
            id: id.clone(),
            source,
        }
    })?;
    let command_access = access_permission.clone();
    let command_modify = modify_permission.clone();
    Ok(
        CommandRegistration::new(id, move |_| command(command_access, command_modify))
            .permission(access_permission),
    )
}

fn invsee_permissions() -> Result<(PermissionExpr, PermissionExpr), PermissionKeyError> {
    let access = PermissionExpr::key(PermissionKey::parse(INVSEE_PERMISSION)?);
    let modify = PermissionExpr::key(PermissionKey::parse(MODIFY_PERMISSION)?);
    Ok((PermissionExpr::Any(vec![access, modify.clone()]), modify))
}

fn command(
    access_permission: PermissionExpr,
    modify_permission: PermissionExpr,
) -> CommandNodeBuilder<CommandSource, SteelCommandRuntime> {
    literal("invsee").then(
        argument("target", SteelArgumentType::player()).executes(move |ctx| {
            let target = ctx.player("target")?;
            let Some(source) = ctx.source().player() else {
                return Err(CommandSyntaxError::dynamic(TextComponent::const_plain(
                    "you cannot use this command from the console",
                )));
            };
            let modify = ctx.source().has_permission(&modify_permission);
            let required_permission = if modify {
                modify_permission.clone()
            } else {
                access_permission.clone()
            };
            source.open_menu(target.display_name(), |container_id, _world| {
                invsee(container_id, source, &target, modify, required_permission)
            });
            Ok(0)
        }),
    )
}

fn invsee(
    container_id: u8,
    source: &Arc<Player>,
    target: &Arc<Player>,
    modify: bool,
    required_permission: PermissionExpr,
) -> Menu {
    let mut b = MenuBuilder::new(&vanilla_menu_types::GENERIC_9X5, container_id);

    let target_ref = ContainerRef::from(target.inventory.clone());
    let target_inventory = if modify {
        b.player_inventory(&target.inventory).all()
    } else {
        readonly_section(&mut b, &target_ref, (9..36).chain(0..9))
    };

    let armor = if modify {
        // Administrative modification is intentionally not constrained by equipment rules.
        b.custom_section(
            [39, 38, 37, 36]
                .map(|index| SlotType::Normal(NormalSlot::new(target_ref.clone(), index))),
            [target_ref.clone()],
        )
    } else {
        readonly_section(&mut b, &target_ref, [39, 38, 37, 36])
    };
    let offhand = if modify {
        b.custom_section(
            [SlotType::Normal(NormalSlot::new(target_ref.clone(), 40))],
            [target_ref.clone()],
        )
    } else {
        readonly_section(&mut b, &target_ref, [40])
    };

    let crafting_handler = target.inventory_crafting_handler();
    let crafting_container = crafting_handler.crafting_container();
    let result_container = crafting_handler.result_container();
    let crafting = if modify {
        // Crafting inputs may leave this menu but never enter through it.
        b.restricted_section(crafting_container, 4, |_, _| false)
    } else {
        b.display_section(crafting_container, 4)
    };
    b.register_container(result_container);

    let target_slots = 0..b.slot_count();
    let viewer = b.player_inventory(&source.inventory);

    if modify {
        let inventories_alias = Arc::ptr_eq(&source.inventory, &target.inventory);
        if !inventories_alias {
            b.route(target_inventory, [viewer.all()], FillDirection::Backward);
            b.route(
                viewer.all(),
                [target_inventory, armor, offhand],
                FillDirection::Forward,
            );
        }
        b.route(
            [armor, offhand, crafting],
            [viewer.all()],
            FillDirection::Backward,
        );
    }

    b.build(MenuKindType::custom(InvseeMenuKind {
        target: Arc::downgrade(target),
        target_domain: target.get_world().domain().into(),
        required_permission,
        modify,
        target_slots,
        crafting,
        crafting_handler,
    }))
}

fn readonly_section(
    builder: &mut MenuBuilder,
    container: &ContainerRef,
    indices: impl IntoIterator<Item = usize>,
) -> Section {
    let may_place: MayPlaceFn = Arc::new(|_, _| false);
    let may_pickup: MayPickupFn = Arc::new(|_, _, _, _| false);
    let slots = indices.into_iter().map(|index| {
        SlotType::Restricted(RestrictedSlot::new(
            container.clone(),
            index,
            may_place.clone(),
            Some(may_pickup.clone()),
        ))
    });
    builder.custom_section(slots, [container.clone()])
}

struct InvseeMenuKind {
    target: Weak<Player>,
    target_domain: Box<str>,
    required_permission: PermissionExpr,
    modify: bool,
    target_slots: Range<usize>,
    crafting: Section,
    crafting_handler: CraftingHandler,
}

impl MenuKind for InvseeMenuKind {
    fn on_open(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.crafting_handler.update_result(guard);
    }

    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.crafting_handler.update_result(guard);
    }

    fn on_slot_clicked(
        &mut self,
        _behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        click: Click,
        _player: &Player,
    ) -> ClickOutcome {
        let Some(slot) = click.slot() else {
            return ClickOutcome::Fallthrough;
        };
        if (!self.modify && self.target_slots.contains(&slot))
            || (self.crafting.contains(slot) && matches!(click, Click::Clone { .. }))
        {
            ClickOutcome::Consume
        } else {
            ClickOutcome::Fallthrough
        }
    }

    fn can_drag_to(&self, slot_index: usize) -> bool {
        if self.modify {
            !self.crafting.contains(slot_index)
        } else {
            !self.target_slots.contains(&slot_index)
        }
    }

    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, slot_index: usize) -> bool {
        self.modify || !self.target_slots.contains(&slot_index)
    }

    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        let Some(target) = self.target.upgrade() else {
            return false;
        };
        player.has_permission(&self.required_permission)
            && !target.connection.closed()
            && !target.is_domain_switching()
            && target.get_world().domain() == self.target_domain.as_ref()
    }
}

#[cfg(test)]
mod tests;
