# Remotes and the message box

A remote is any machine you can reach with `ssh`. Each one gets a plain directory, its box, holding one JSON file per message. reeve pushes messages into the remote's box and pulls what the remote wrote, always starting the connection from this machine. Agents, scripts and people read and write messages; reeve only moves them. It never reads a body for meaning, and no message starts anything here.

Native option checked: `ssh` and plain files do the transport. reeve adds what they lack: a queue for when the connection is down, ids so a retry never duplicates, and one inbox to read.

```sh
reeve remote add myserver me@myserver.example.net   # saves config only; nothing is contacted
reeve remote init myserver                          # makes ~/.reeve/box/ there, nothing else
reeve send hermes@myserver "CI is red on main, can you look?"
reeve remote sync                                   # push what is queued, pull what arrived
reeve inbox                                         # what arrived; `show ID`, `done ID`
```

ssh must already work without a prompt (a key in your agent, and the host known). reeve passes `BatchMode=yes` and a 60 second limit per connection.

## Commands

| Command | What it does |
| --- | --- |
| `remote add NAME TARGET [--box PATH] [--owner-channel known\|unknown]` | Saves the remote in `~/.config/reeve/remotes.json`. TARGET is a host, `user@host` or an alias from your ssh config |
| `remote ls` | State of the last contact, how many messages are queued, the owner channel |
| `remote check NAME` | Read-only: ssh, the box, the tools it needs |
| `remote init NAME` | Makes the box |
| `remote sync [NAME]` | Pushes the queue, oldest first, then pulls the outbox |
| `remote rm NAME [--force]` | Forgets the remote; refuses while messages are queued unless `--force` |
| `send TO TEXT [--reply-to ID] [--ttl 24h] [--from NAME]` | TO is `name` (here) or `name@remote`. `-` as TEXT reads stdin |
| `inbox [--from X] [--unanswered] [--all]`, `inbox show ID`, `inbox done ID` | Reads local files only |

`reeve doctor` has a line for remotes and for messages stuck in the queue. It contacts no one.

## The box

```
~/.reeve/box/
  inbox/    messages for the agent there (reeve writes, the agent reads)
  outbox/   messages from the agent (it writes, reeve pulls)
  seen/     one empty marker per id reeve delivered
```

The box is reeve's own, apart from any agent's mailbox or memory, and mode 0700. `--box PATH` puts it elsewhere, for an agent that runs as another user: the ssh user needs write access, and reeve never uses sudo. The remote needs a POSIX `sh` with `mkdir`, `mv`, `cat`, `rm` and `wc`, and `find` to prune old markers. No reeve binary, no python.

## A message

```json
{
  "v": 1,
  "id": "20261029T140311Z-9f3a1c",
  "from": "hermes@myserver",
  "to": "me",
  "at": "2026-10-29T14:03:11Z",
  "reply_to": "20261029T135900Z-04be77",
  "expires_at": "2026-10-30T14:03:11Z",
  "body": "free text, markdown allowed, up to 64 KiB"
}
```

`reply_to` and `expires_at` are optional. The id is a sortable UTC time and a random part, only `A-Za-z0-9-`, and the file is `ID.json`. reeve checks the JSON, these fields, the id and the size, and carries any other key along untouched. A conversation is the `reply_to` chain; a PR link goes in the body. "Unanswered" is derived: an incoming message that no message sent from here answers with `--reply-to`.

## Delivery

- **Atomic writes.** A writer writes `.tmp-ID`, then renames to `ID.json`. A reader ignores dotfiles.
- **Push.** One ssh runs a fixed script that writes the message, checks its length, and renames it into `inbox/` unless `seen/ID` already exists. It answers `ok` or `dup`. The queued copy here is deleted only after one of the two, so a lost answer costs a repeat that the remote recognises. The agent may move what it read (to `inbox/.read/`) without causing a resend.
- **Pull.** reeve lists `outbox/*.json`, up to 100, reads each one it doesn't have, checks it, writes it here by temp file and rename, and only then moves it on the remote to `outbox/.done/`. A crash anywhere repeats a step and loses nothing. A file that isn't a valid message is moved to `outbox/.rejected/` and counted, so it doesn't come back every time.
- **Offline.** `send` always succeeds here. With the remote down the message waits in `~/.reeve/spool/NAME/` with its final id and goes out on a later `send` or `remote sync`. A `send` right after a failed attempt just queues instead of waiting on ssh again. Nothing else in reeve waits on a remote.

States reeve knows: queued (here), delivered (in the remote's inbox), and read only if the agent moved the file, visible at the next sync. Whether the agent acted is what it writes back.

Seen markers, delivered copies and read messages are kept 30 days on both sides.

## Safety

- **A message never starts anything here.** No field is a command, a path, a hook or a skill, and nothing answers or runs because one arrived. An agent sees one only when it chooses to read the inbox.
- **Identity is the box.** reeve stamps each pulled message with the remote it came through (`via`) and replaces any `via` the sender wrote. A `from` that doesn't match is shown as `via myserver, claims x@elsewhere`. Treat all remote text as untrusted data.
- **No message text in a shell.** Every remote command is one fixed script. Its arguments are ids, a length and the box path, each checked and quoted, and message text goes only through stdin. Terminal escape characters in remote text are replaced when printed.
- **Limits.** 64 KiB per body, 100 messages per pull, 5000 files in the local inbox; the rest waits on the remote.
- **No secrets in the box.** reeve stores no keys; ssh uses your agent and config. The events log and `reeve feedback` hold the command name and a fixed error kind (`remote_unreachable`, `remote_timeout`, `box_invalid`, `no_remote`), never a host, remote name, id or text.
- **Owner channel.** Whether the agent on a remote can reach you another way (Telegram, say) is its own business. You can set `--owner-channel known`; the default `unknown` is shown in `remote ls` and `remote check`. A box is a mailbox, not an escalation policy: when the channel is unknown, whoever follows your messages has to poll `reeve inbox`.

Hermes-style agents whose session has no file tools can't read the box; that needs a session with file tools, or a scheduled job on that side.
