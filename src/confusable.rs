use std::{borrow::Cow, collections::HashMap};

use once_cell::sync::OnceCell;

static CONFUSABLE_MAP: OnceCell<HashMap<char, String>> = OnceCell::new();

fn confusables() -> &'static HashMap<char, String> {
    CONFUSABLE_MAP.get_or_init(|| {
        let confusable_str = include_str!("confusable_data.txt");
        let mut map = HashMap::new();

        for line in confusable_str.lines() {
            if line.starts_with('#') {
                continue;
            }

            if !line.contains(';') {
                continue;
            }

            let parts: Vec<_> = line.split(';').collect();

            let from = parts[0].trim();
            let to = parts[1].trim();

            let from = u32::from_str_radix(from, 16).unwrap();
            let from = char::from_u32(from).unwrap();

            let mut to_buffer = String::new();
            for part in to.split(' ') {
                let part = u32::from_str_radix(part, 16).unwrap();
                let part = char::from_u32(part).unwrap();
                to_buffer.push(part);
            }

            map.insert(from, to_buffer);
        }

        map
    })
}

#[tracing::instrument]
pub fn skeletonize(str: &str) -> Cow<str> {
    let mut result = Cow::Borrowed(str);
    let confusables = confusables();

    for (index, char) in str.char_indices() {
        if matches!(result, Cow::Borrowed(_)) {
            if !confusables.contains_key(&char) {
                // Don't need to make any changes: this character isn't confusable.
                continue;
            } else {
                // Right now, `result` is the original string in full.
                // We want to only include the unconfusable characters that preceded this one.
                // Reassign result here. We'll copy this slice of the string in the next if statement.
                result = Cow::Borrowed(&str[0..index]);
            }
        }

        if let Some(to) = confusables.get(&char) {
            result.to_mut().push_str(to);
        } else {
            // This branch will only be executed if we've already copied the string, in which case
            // we need to append the unconfusable character to the copy.
            result.to_mut().push(char);
        }
    }

    result
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_skeletonize() {
        assert_eq!(skeletonize("ρɑɣρɑl"), "paypal");
        assert_eq!(skeletonize("paɣρɑl"), "paypal");
    }

    #[test]
    fn dont_copy_if_no_confusables() {
        assert_eq!(skeletonize("paypal"), Cow::Borrowed("paypal"));
    }
}
