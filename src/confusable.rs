use std::collections::HashMap;

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

            if !line.contains(";") {
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

pub fn skeletonize(str: &str) -> String {
    let mut buffer = String::with_capacity(str.len());
    let confusables = confusables();

    for (_index, char) in str.char_indices() {
        if let Some(to) = confusables.get(&char) {
            buffer.push_str(to);
        } else {
            buffer.push(char);
        }
    }

    buffer
}


#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_skeletonize() {
        assert_eq!(skeletonize("ρɑɣρɑl"), "paypal");
    }
}
