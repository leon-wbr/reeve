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
| `remote add NAME TARGET [--box PATH] [--owner-channel known\|unknown] [--wake CMD]` | Saves the remote in `~/.config/reeve/remotes.json`. TARGET is a host, `user@host` or an alias from your ssh config |
| `remote ls` | State as of the last contact, when it was, what is queued and for how long, unread messages, the owner channel |
| `remote check NAME` | Read-only: ssh, the box, the tools it needs |
| `remote init NAME` | Makes the box |
| `remote sync [NAME] [--watch [--every SECS]]` | Pushes the queue, oldest first, then pulls the outbox. `--watch` keeps doing it |
| `remote rm NAME [--force]` | Forgets the remote; refuses while messages are queued unless `--force` |
| `send TO TEXT [--reply-to ID] [--ttl 24h] [--from NAME]` | TO is `name` (here) or `name@remote`. `-` as TEXT reads stdin |
| `inbox [--from X] [--unanswered] [--all]`, `inbox show ID`, `inbox done ID` | Reads local files only |

`reeve doctor` has a line for remotes and for messages stuck in the queue. It contacts no one. `reeve ls` adds a row per remote, from the last contact, and opens no connection either.

## The box

```
~/.reeve/box/
  inbox/    messages for the agent there (reeve writes, the agent reads)
  outbox/   messages from the agent (it writes, reeve pulls)
  seen/     one empty marker per id reeve delivered
  status.json   optional heartbeat, see below
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

### Watching, and a connection that drops

`reeve remote sync --watch` is not a daemon: run it in a background shell or Claude Code's Monitor tool, like `reeve watch`. It syncs every 15 seconds (`--every`). When a remote is unreachable it waits 2, 5, 15, then 60 seconds, with ±20% jitter, per remote, and keeps trying until you stop it. It prints a line per change, not per pass:

```
watching myserver every 15s
myserver: unreachable; retrying in 2s
myserver: back online
myserver: delivered 2
myserver: new message 20261029T140311Z-9f3a1c (`reeve inbox show 20261029T140311Z-9f3a1c`)
```

The lines hold remote names, ids and fixed words, never what a remote wrote or ssh's own text, since agents read them. A drop never loses or repeats a message, whenever it comes: before a command runs, after it ran but before its answer arrived, or with a push cut off halfway (the remote checks the length and commits nothing). The next pass repeats the step, and `seen/` and the local inbox make the repeat a no-op. The tests cut the connection at every step of a sync in each of these ways.

### Expiry

`expires_at` is compared on the machine that looks, using the writer's UTC time. A queued message that expires before it could be sent moves to `~/.reeve/spool/NAME/.expired/` and leaves a note in the inbox from `reeve@local` (`reply_to` is the message), so a silent loss can't happen. This needs no connection, so it works while the remote is down. A message without `expires_at` waits as long as it takes. A pulled message that has expired still lands in the inbox, marked `[expired]`, so you can see what was missed.

### Wake

`--wake CMD` is a command for the remote's shell, which reeve runs over ssh after a delivery, for example a script that tells the agent there to look in the box. Without it the agent polls its inbox, which works anywhere there is a file tool. It runs on the remote, never here. A failed wake never fails the delivery; it is retried at the next sync until it works. Hermes-style setups need a one-time configuration on their side first; that is not reeve's to change.

### Heartbeat

If the box has a `status.json` (one line of JSON with an `at` UTC time, written by whatever supervises the agent, say every 30 seconds), each sync reads it with the listing. The state in `remote ls` and `reeve ls` is `up` if the beat was within 90 seconds of that contact, `stale` if not, `reachable` if the box has no heartbeat, `unreachable` if the latest attempt failed (an old beat never means alive) and `unknown` before any contact. Nothing asks the remote when you list; the state is as of the last contact, shown in the age column.

Seen markers, delivered copies and read messages are kept 30 days on both sides.

## Safety

- **A message never starts anything here.** No field is a command, a path, a hook or a skill, and nothing answers or runs because one arrived. An agent sees one only when it chooses to read the inbox.
- **Identity is the box.** reeve stamps each pulled message with the remote it came through (`via`) and replaces any `via` the sender wrote. A `from` that doesn't match is shown as `via myserver, claims x@elsewhere`. Treat all remote text as untrusted data.
- **No message text in a shell.** Every remote command is one fixed script. Its arguments are ids, a length and the box path, each checked and quoted, and message text goes only through stdin. Terminal escape characters in remote text are replaced when printed.
- **Limits.** 64 KiB per body, 100 messages per pull, 5000 files in the local inbox; the rest waits on the remote.
- **No secrets in the box.** reeve stores no keys; ssh uses your agent and config. The events log and `reeve feedback` hold the command name and a fixed error kind (`remote_unreachable`, `remote_timeout`, `box_invalid`, `no_remote`), never a host, remote name, id or text.
- **Owner channel.** Whether the agent on a remote can reach you another way (Telegram, say) is its own business. You can set `--owner-channel known`; the default `unknown` is shown in `remote ls` and `remote check`. A box is a mailbox, not an escalation policy: when the channel is unknown, whoever follows your messages has to poll `reeve inbox`.

Hermes-style agents whose session has no file tools can't read the box; that needs a session with file tools, or a scheduled job on that side.
