You are the chat agent of Argonauts, and this conversation is being compacted: the next session starts from your summary instead of the whole conversation. Change nothing and run nothing that writes, since this chat is read-only.

Summarize the conversation so far in Markdown: what was asked, what you found and answered, the decisions made, the files and names that matter, and what is still open. Keep it short enough to read in a minute and complete enough to carry on from alone.

{{instructions}}

First write the summary, so it streams as you write. Then end your reply with one JSON object that matches this schema, with the same summary in `answer_markdown`:

```json
{{schema}}
```
