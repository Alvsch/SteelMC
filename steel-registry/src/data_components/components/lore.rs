use simdnbt::{
    FromNbtTag, ToNbtTag,
    borrow::NbtTag as BorrowedNbtTag,
    owned::{NbtList, NbtTag as OwnedNbtTag},
};
use steel_utils::codec::VarInt;
use steel_utils::hash::{ComponentHasher, HashComponent};
use steel_utils::serial::{ReadFrom, WriteTo};
use text_components::TextComponent;

/// Maximum number of lore lines, matching vanilla's `ItemLore.MAX_LINES`.
pub const MAX_LORE_LINES: usize = 256;

/// The `minecraft:lore` component: extra tooltip lines shown on an item.
///
/// Vanilla also stores pre-styled lines (italic dark purple), but styling is
/// applied client-side when rendering the tooltip, so only the raw lines are
/// kept here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemLore {
    pub lines: Vec<TextComponent>,
}

impl ItemLore {
    #[must_use]
    pub fn new(lines: Vec<TextComponent>) -> Self {
        Self { lines }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

/// Network format: VarInt count, then each line as an NBT-encoded text component.
impl WriteTo for ItemLore {
    fn write(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        VarInt(self.lines.len() as i32).write(writer)?;
        for line in &self.lines {
            line.write(writer)?;
        }
        Ok(())
    }
}

impl ReadFrom for ItemLore {
    fn read(data: &mut std::io::Cursor<&[u8]>) -> std::io::Result<Self> {
        let count = VarInt::read(data)?.0;
        if !(0..=MAX_LORE_LINES as i32).contains(&count) {
            return Err(std::io::Error::other(format!(
                "Lore line count out of range: {count}"
            )));
        }
        let mut lines = Vec::with_capacity(count as usize);
        for _ in 0..count {
            // Symmetric with `TextComponent`'s WriteTo: a bare NBT tag
            // (id + payload), no length prefix.
            let tag = simdnbt::owned::read_tag(data).map_err(|e| {
                std::io::Error::other(format!("Failed to read lore line NBT: {e:?}"))
            })?;
            let line = text_from_tag(&tag).ok_or_else(|| {
                std::io::Error::other("Failed to parse lore line as text component")
            })?;
            lines.push(line);
        }
        Ok(Self { lines })
    }
}

/// NBT format: list of text components (plain lines collapse to strings).
impl ToNbtTag for ItemLore {
    fn to_nbt_tag(self) -> OwnedNbtTag {
        OwnedNbtTag::List(NbtList::from(
            self.lines
                .into_iter()
                .map(ToNbtTag::to_nbt_tag)
                .collect::<Vec<_>>(),
        ))
    }
}

impl FromNbtTag for ItemLore {
    fn from_nbt_tag(tag: BorrowedNbtTag) -> Option<Self> {
        let list = tag.to_owned().into_list()?;
        let tags = list.as_nbt_tags();
        if tags.len() > MAX_LORE_LINES {
            return None;
        }
        let mut lines = Vec::with_capacity(tags.len());
        for tag in &tags {
            lines.push(text_from_tag(tag)?);
        }
        Some(Self { lines })
    }
}

/// Parses a single lore line tag. `TextComponent::from_nbt` rejects empty
/// strings, but an empty string is a valid blank lore line.
fn text_from_tag(tag: &OwnedNbtTag) -> Option<TextComponent> {
    if is_blank_line(tag) {
        return Some(TextComponent::new());
    }
    TextComponent::from_nbt(tag)
}

/// A blank line is an empty string, or an empty string wrapped as `{"": ""}` —
/// the marker compound vanilla's `ListTag` uses for mismatched elements in
/// heterogeneous lists.
fn is_blank_line(tag: &OwnedNbtTag) -> bool {
    match tag {
        OwnedNbtTag::String(s) => s.to_str().is_empty(),
        OwnedNbtTag::Compound(c) if c.len() == 1 => {
            matches!(c.get(""), Some(OwnedNbtTag::String(s)) if s.to_str().is_empty())
        }
        _ => false,
    }
}

impl HashComponent for ItemLore {
    fn hash_component(&self, hasher: &mut ComponentHasher) {
        hasher.start_list();
        for line in &self.lines {
            line.hash_component(hasher);
        }
        hasher.end_list();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nbt_round_trip_plain_lines() {
        let lore = ItemLore::new(vec![
            TextComponent::plain("first line"),
            TextComponent::plain("second line"),
        ]);

        let tag = lore.clone().to_nbt_tag();
        let mut compound = simdnbt::owned::NbtCompound::new();
        compound.insert("lore", tag);
        let mut bytes = Vec::new();
        simdnbt::owned::BaseNbt::new("", compound).write(&mut bytes);

        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let nbt = simdnbt::borrow::read(&mut cursor).unwrap();
        let nbt = nbt.unwrap();
        let parsed = ItemLore::from_nbt_tag(nbt.get("lore").unwrap()).unwrap();
        assert_eq!(parsed, lore);
    }

    #[test]
    fn nbt_round_trip_mixed_styled_and_blank_lines() {
        use text_components::{Modifier, format::Color};

        // A styled line forces the compound-list encoding, so the blank line
        // is written as vanilla's `{"": ""}` wrapper.
        let lore = ItemLore::new(vec![
            TextComponent::plain("plain"),
            TextComponent::new(),
            TextComponent::plain("styled").color(Color::Red),
        ]);

        let tag = lore.clone().to_nbt_tag();
        let mut compound = simdnbt::owned::NbtCompound::new();
        compound.insert("lore", tag);
        let mut bytes = Vec::new();
        simdnbt::owned::BaseNbt::new("", compound).write(&mut bytes);

        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let nbt = simdnbt::borrow::read(&mut cursor).unwrap();
        let nbt = nbt.unwrap();
        let parsed = ItemLore::from_nbt_tag(nbt.get("lore").unwrap()).unwrap();
        assert_eq!(parsed, lore);
    }

    #[test]
    fn network_round_trip() {
        let lore = ItemLore::new(vec![
            TextComponent::plain("a"),
            TextComponent::new(),
            TextComponent::plain("b"),
        ]);

        let mut bytes = Vec::new();
        lore.write(&mut bytes).unwrap();
        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        let parsed = ItemLore::read(&mut cursor).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(cursor.position() as usize, bytes.len());
    }

    #[test]
    fn network_read_rejects_oversized_count() {
        let mut bytes = Vec::new();
        VarInt(MAX_LORE_LINES as i32 + 1).write(&mut bytes).unwrap();
        let mut cursor = std::io::Cursor::new(bytes.as_slice());
        assert!(ItemLore::read(&mut cursor).is_err());
    }

    #[test]
    fn plain_lines_hash_as_string_list() {
        // Plain text components collapse to strings in vanilla's codec, so the
        // hash must equal a plain string-list hash.
        let lore = ItemLore::new(vec![TextComponent::plain("hello")]);
        let mut hasher = ComponentHasher::new();
        lore.hash_component(&mut hasher);

        let mut expected = ComponentHasher::new();
        expected.start_list();
        expected.put_string("hello");
        expected.end_list();

        assert_eq!(hasher.finish(), expected.finish());
    }
}
