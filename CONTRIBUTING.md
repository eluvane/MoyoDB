# Contributing

Contributions for MoyoDB 1.0.0 and later are governed by the [Mother of Licenses 1.0](LICENSE), including the upstream workflow in Section 6, the contribution terms in Section 9, and the patent terms in Section 10. Read these terms before submitting work.

Submit changes to the official [eluvane/MoyoDB repository](https://github.com/eluvane/MoyoDB). Technical forks, review branches, patches, and temporary test artifacts are permitted for bona fide upstream review under Section 6. Identify their contribution-only purpose and the official upstream project; do not publish them as general-use releases or an independent product. Private modifications may continue under Section 3 after the contribution process ends.

Contributors retain ownership of their original work. Section 9 grants the licensor, Eluvane, broad, irrevocable, royalty-free copyright permissions, including commercial use, sublicensing, transfer, and relicensing. Section 10 governs patent permissions. These grants do not entitle contributors to payment or revenue sharing unless separately agreed in writing or required by applicable law.

By knowingly submitting a Contribution with notice of Section 9, you accept its contribution terms. Contribution and patent grants remain governed by Sections 9 and 10. The default workflow does not require a separate acceptance statement or checkbox in the pull request or an accompanying message.

Submit only material you own or are authorized to contribute. Identify known third-party material and its license terms in the submission. Material you cannot license under these terms requires a separate arrangement with the licensor before incorporation.

For licensing questions or a separate agreement, contact [keiko1337@proton.me](mailto:keiko1337@proton.me).

## Opening a pull request

Keep each pull request focused on one concrete change. Use the [pull request template](.github/PULL_REQUEST_TEMPLATE.md), and keep unrelated cleanup and local build output out of the diff. The title and description should describe the final change for a reviewer who has not seen the development conversation.

### Title

Write a short, descriptive title in English, starting with an action and naming the affected behavior. For example, `Fix OPFS recovery after an interrupted checkpoint` or `Document SDK transaction boundaries`. Describe the concrete result; avoid generic titles and broad completion claims.

### Description

Use the prose style of [ouro pull request #118](https://github.com/eluvane/ouro/pull/118): compact English paragraphs that describe the final behavior, its implementation and material limits. Start directly with the concrete change and its effect. Use one or two connected paragraphs for most changes, grouping related behavior in the same paragraph. Add another paragraph only when a distinct part of the change needs it. Include a concrete trigger or before/after example when it makes the result clearer.

Mention material compatibility, durability, security, performance, or release impact and limitations only when they apply. Breaking changes need migration guidance. Describe the final implementation and keep the title and description aligned with its scope; omit conversational history and abandoned approaches unless they explain a relevant tradeoff.

Do not add description headings such as `Summary`, `Changes`, `Testing` or `Contribution terms`, bullet inventories, technical checklists or a list of validation commands. Omit empty template scaffolding and unsupported claims such as “fully safe” or “production-ready.” Keep detailed commands and execution evidence in CI output or the accompanying task report. If a missing or failed check materially limits confidence in the change, state that limitation briefly in the prose.

Keep the pull request description as prose only, matching ouro #118. Do not append license-acceptance statements, contribution checkboxes or other boilerplate footers. Identify third-party material and its license terms in the prose when applicable; do not add a declaration when no such material is included.

### Coding agents

Follow the same title and description rules, including the ouro #118 prose format. Describe the final diff for a reviewer who has not read the conversation; do not replace the requested prose with a sectioned report. Create branches and pull requests only when requested, preserve any supplied branch, title, and base, and do not merge without an explicit maintainer instruction. Local documentation or code edits do not by themselves authorize publishing a pull request.

## Markdown style

Use visible dashes for unordered prose items in Markdown files and pull request descriptions. Escape the marker with a backslash (`\-`), and use a trailing backslash between adjacent items so GitHub keeps each item on its own line:

```markdown
\- First item\
\- Second item
```

Preserve task-list checkboxes, numbered steps, tables, and code examples in their native syntax. Report generators must follow the same dash convention in their Markdown output.
