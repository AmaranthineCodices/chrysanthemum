# chrysanthemum

[![CI](https://github.com/AmaranthineCodices/chrysanthemum/actions/workflows/ci.yml/badge.svg)](https://github.com/AmaranthineCodices/chrysanthemum/actions/workflows/ci.yml)

Chrysanthemum is a message filtering Discord bot developed for the [unofficial Roblox Discord server](https://discord.gg/roblox).

## Configuration
Chrysanthemum requires a bot token passed either via a `.env` file or the `DISCORD_TOKEN` environment variable.

The configuration file is loaded from `chrysanthemum.cfg.json`, and is structured as follows:

```json
{
    "guilds": {
        "<GUILD_ID>": {
            "filters": [
                {
                    "actions": [
                        {
                            "action": "delete"
                        },
                        {
                            "action": "send_message",
                            "channel_id": "<CHANNEL_ID>",
                            "content": "$USER_ID sent a filtered message: $REASON\n```$MESSAGE_CONTENT```"
                        }
                    ],
                    "spam": {
                        "emoji": {
                            "count": 5,
                            "interval": 30
                        }
                        // duplicates, links, attachments are in the same format.
                        // actions taken are defined in `actions` above.
                    },
                    // If both exclude_channels and include_channels are specified,
                    // include_channels takes priority.
                    "exclude_channels": [],
                    "include_channels": [],
                    "exclude_roles": [],
                    "rules": [
                        // RULES
                    ]
                }
            ]
        }
    }
}
```
Each guild contains a `filters` property, which is an array of filter configurations. Each filter configuration has the following properties:

* `rules`
* `actions`
* `spam`
* `exclude_channels`
* `include_channels`
* `exclude_roles`

### Rules
Each filter configuration allows you to declaratively specify rules to filter messages on. If any rule matches a new message's content, the actions specified will be applied to the message. There are currently seven kinds of filters, with more coming soon.

#### Words
```json
{
    "type": "words",
    "words": [
        "<WORD>"
    ]
}
```
The `words` filter searches for disallowed words within a message. A word is separated from other text with whitespace.

#### Regex
```json
{
    "type": "regex",
    "regexes": [
        "a[0-9]b"
    ]
}
```
The `regex` filter checks that a message doesn't match any of the provided regexes.

#### Zalgo
```json
{
    "type": "zalgo"
}
```
The `zalgo` filter checks for Zalgo text (z̵̼͠a̶̢͎͆͊l̷̬͠g̷̡͇͒o̶̘̓).

#### MIME type
```json
{
    "type": "mime_type",
    "mode": "allow",
    "types": [
        "image/png",
        "image/jpeg",
        "image/gif"
    ],
    "allow_unknown": false
}
```
The `mime_type` filter checks attachment MIME types. The `mode` field controls the behavior of the filter - `allow` means it denies content types that aren't in the list, while `deny` means it denies content types that _are_ in the list. `allow_unknown` controls the behavior of the filter when the Discord API doesn't return a content type - `true` means that attachments without a content type are allowed, and `false` means that they are denied.

#### Link
```json
{
    "type": "link",
    "mode": "allow",
    "domains": [
        "discord.com"
    ]
}
```
The `link` filter checks the domains of links included in a message. The `mode` field controls the behavior of the filter - `allow` means it denies domains that aren't in the list, while `deny` means it denies domains that _are_ in the list.

#### Invite
```json
{
    "type": "invite",
    "mode": "allow",
    "invites": [
        "roblox"
    ]
}
```
The `invite` filter checks for invite codes in a message. The `mode` field controls the behavior of the filter - `allow` means it denies invite codes that aren't in the list, while `deny` means it denies invite codes that _are_ in the list.

#### Stickers
```json
{
    "type": "sticker",
    "mode": "allow",
    "stickers": []
}
```
The `sticker` filter checks for stickers sent with the message. The `mode` field controls the behavior of the filter - `allow` means it denies stickers that aren't in the list, while `deny` means it denies stickers that _are_ in the list.

### Actions
Chrysanthemum supports configuring which actions to take when a message is filtered. Actions look like this in the configuration file:
```json
{
    "action": "<ACTION_TYPE>",
    "<ACTION_PARAMETER>": ""
}
```


#### `delete`
```json
{
    "action": "delete"
}
```

The `delete` action deletes the filtered message. It takes no parameters.

#### `send_message`
```json
{
    "action": "send_message",
    "channel_id": "<CHANNEL_ID>",
    "content": "$USER_ID sent a bad message: $REASON\n```$MESSAGE_CONTENT```"
}
```
The `send_message` action sends a message to a channel when a message is filtered. It takes two parameters: `channel_id`, the channel to send the message to, and `content`, the message content. There are three template variables that can be used in `content`:

* `$USER_ID`: The ID of the user who sent the message.
* `$REASON`: Why the message was filtered.
* `$MESSAGE_CONTENT`: The content of the filtered message.

### Spam
```json
"spam": {
    "emoji": {
        "count": 5,
        // 30 seconds
        "interval": 30
    },
    "links": {
        "count": 5,
        "interval": 30
    },
    "attachments": {
        "count": 5,
        "interval": 30
    },
    "duplicates": {
        "count": 5,
        "interval": 30
    }
},
```
Chrysanthemum supports spam filtering. It **does not** filter by message frequency; to guard against this, consider enabling [slowmode](https://support.discord.com/hc/en-us/articles/360016150952-Slowmode-Slllooowwwiiinng-down-your-channel) in the channels. Chrysanthemum's spam filtering guards against the following kinds of spam:

* Excessive emojis
* Excessive links
* Excessive attachments
* Duplicate messages

All of these can be configured via the `spam` filter configuration object. All behave in the same fashion. To disable any component of this functionality, omit the configuration section.


### Excluding / including channels
```json
"exclude_channels": [
    "<CHANNEL_ID>"
]
```
```json
"include_channels": [
    "<CHANNEL_ID>"
]
```
There are likely some channels that you don't want Chrysanthemum to look at within a guild. For optimal performance of Chrysanthemum, you should use role permissions to exclude Chrysanthemum from these channels entirely. However, in case you need to filter these at the configuration level, Chrysanthemum allows you to specify `exclude_channels` or `include_channels` in the configuration file.

If both of these fields have channel IDs in them, `include_channels` overrides `exclude_channels` - the contents of `exclude_channels` will be **ignored**. Chrysanthemum will print a message to the log when starting up if this is the case.

### Excluding roles
```json
"exclude_roles": [
    "<ROLE_ID>"
]
```
It may be desirable for some roles to be exempt from Chrysanthemum's filtering, like moderators and other bots. To do this, specify the `exclude_roles` field in the filter configuration:
