# Оптимизации MoyoDB: проходы V и VI

В проходе VI реализованы все 26 пунктов прежней очереди, включая небольшие
улучшения. Перед изменениями добавлялись небольшие проверки сокращаемой работы
и независимые проверки результата, ошибок, TTL или владения буферами.
Бенчмарки и тяжёлые прогоны не запускались по ограничению пользователя.
Число backend calls, копий и allocations не означает измеренное ускорение
по времени или уменьшение фактических дисковых IOPS.

## Сохранённые изменения прохода V

До изменения production-кода добавлены проверки следующих случаев:

| Путь                                                | Лишняя работа на исходном коде                                                         | Проверка корректности                                                                     |
| --------------------------------------------------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| Повторная запись в staged store, `engine.rs`        | 256 копий имени; 63 232 выделенных байта для имени длиной 240 против 2 048 для длины 1 | Прочитанное значение и rollback; существующие transaction/batch tests                     |
| Кодирование snapshot, `snapshot.rs`                 | 307 аллокаций для 256 записей, включая копии ключей для проверки дубликатов            | Точные байты v3, порядок входа, TTL, границы ключей и ошибки дубликатов                   |
| Освобождение дерева, `btree.rs::free_tree`          | 1 251 аллокация / 444 776 байт для 512 inline значений по 769 байт                     | Точное множество освобождённых страниц, исходные bytes, malformed cells/children/overflow |
| Ограниченный RW scan, `engine.rs::scan_with_staged` | Чтение следующего листа после заполнения результата                                    | RO/RW read-offset oracle, оба направления, staged override/delete, TTL и bounds           |
| Результат indexed scan, `worker.ts`                 | Вторая копия каждого уже независимо декодированного primary key                        | Byte oracle, caller-buffer ownership, повторный scan/get после изменения выданного ключа  |

Эти счётчики измеряют allocator requests, backend calls и явные копии, а не
время выполнения, live memory или физические обращения диска. Итоги запусков
хранятся отдельно в локальном кэше; таблица фиксирует исходные причины работы.

## Реализованные 26 пунктов прохода VI

Таблица сохраняет исходные причины аудита и проверяемые контракты. Все пункты
реализованы; наблюдаемые результаты лёгких проверок перечислены ниже.

| Приоритет                  | Путь и предлагаемое изменение                                                                | Ожидаемое сокращение работы                                                                    | Что проверить до правки                                                                                                                             |
| -------------------------- | -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| Высокий                    | `btree.rs::collect_leaf_window`: выдавать только востребованные строки листа                 | Копии ключей/inline values за пределами `limit`; сейчас клонируется весь подходящий лист       | Forward/reverse, TTL stale prefix, staged merge, существующий порядок обнаружения повреждённых cells; сравнить полный scan                          |
| Высокий                    | `engine.rs::get_many`: группировать одинаковые ключи и общие ветви дерева                    | Повторные descents и overflow reads; сейчас каждый ключ начинает с root                        | Permuted/duplicate/missing keys, cache=1, один timestamp, staged lifecycle, исходный порядок результатов и ошибок                                   |
| Высокий                    | `wal.rs::replay_wal_index`: объединять соседние исходные WAL spans                           | На 130 соседних page images целевой read count 130 → 3; дополнительные record headers менее 1% | Latest-write-wins, gaps, границы 64/65, short/error reads, durable byte oracle, ошибка до manifest/truncate                                         |
| Высокий                    | `recovery.rs` / WAL scanner: commit visitor вместо удержания всей истории                    | Metadata с O(records + transactions) до O(distinct pages + largest transaction)                | Повреждённый старый commit нельзя скрыть поздним overwrite; torn tail, published prefix, idempotence; replay только после полного scan              |
| Высокий                    | `storage/opfs.rs` / `opfs_shim.js`: read прямо в mutable WASM slice                          | JS временный buffer и последующая копия `bytes.to_vec()`                                       | Generated glue действительно передаёт view без копии; short reads, bounds, detached/grown memory, настоящие OPFS tests                              |
| Высокий                    | ChangeFeed WASM binding: typed bytes вместо промежуточных `number[]`                         | Поэлементная serde-сериализация и повторный перевод key/value в Uint8Array                     | Реальный WASM wire shape, Put/Delete/Clear/Drop, optional value, compression и ownership; CHG1 format сохраняется                                   |
| Высокий                    | `worker-protocol.ts`: compact-copy partial views одиночных бинарных аргументов               | Structured clone большого backing buffer для короткого subarray                                | Настоящий structuredClone с fake transport, offsets, overlapping key/value, caller не detached, повторное использование; SharedArrayBuffer отдельно |
| Средний                    | `engine.rs::compact_into`: owned adjacent page batches                                       | N page-cache allocations и N backend writes; concat buffer всё ещё копирует bytes              | Durable bytes, gaps, bounded scratch, write failure/retry, flush перед publication; не заявлять устранение всех копий                               |
| Средний                    | `snapshot.rs::decode_snapshot`: borrowed seen keys/names                                     | Копия каждого ключа и имени для duplicate sets                                                 | Lifetime ссылок из input, unsorted entries, duplicates, malformed length/expiry, decode format oracle                                               |
| Средний                    | `engine.rs::changes_since` / `change_feed.rs`: borrowed validation до store filter           | Копии key/value исключённых магазинов                                                          | Полная валидация исключённых payload и прежний порядок corruption; cursor/floor/limit/store filter                                                  |
| Средний                    | `worker.ts::changesSince`: ограничить output materialization при `limit`                     | Сейчас без stores фильтра SDK обрабатывает весь хвост, затем slice(limit), даже для limit=0    | Сохранить нынешнюю проверку поздних malformed records; считать выходные objects и key copies отдельно от decode                                     |
| Средний                    | `btree.rs::collect_keys_below`: keys-only iteration для pruning                              | Inline values changelog копируются, затем отбрасываются при сборе только ключей                | Одинаковый pruning floor, limit/batch boundary, порядок и прежние cell validation errors                                                            |
| Средний                    | `btree.rs::read_node` / `merge_leaf`: не копировать заменяемые старые values                 | Старый inline payload, который overwrite/delete сразу отбрасывает                              | Bytes новых страниц, untouched keys, overflow retirement и error order; не кэшировать TTL baseline bool                                             |
| Средний                    | Point-read envelope path: payload сразу в итоговый Vec                                       | Повторный сдвиг V bytes через `decode_owned_for_store` / `drain(..16)`                         | Raw/system/enveloped inline и overflow; пустой payload; полная проверка overflow tail даже у expired value                                          |
| Средний                    | `scan_with_staged`: пропуск полного TTL sweep, если expiring mutations отсутствуют           | O(staged mutations) перед каждым узким scan, включая limit=0                                   | Счётчик просмотренных mutations; expired staged Put над живым base, rollback, clock reversal, commit cleanup                                        |
| Средний                    | `worker.ts::reconcileIndexes`: сократить повторное чтение source-store                       | M scans/decompress/JSON passes для M индексов одного store                                     | TTL между индексами, порядок ошибок index/row, unique conflicts, compressed sources, rollback и old readers; простое memoize меняет наблюдение TTL  |
| Средний                    | `worker.ts::assertUniqueIndexAvailability`: bounded exact-range paging                       | Все физические rows до первого живого конфликта                                                | Stale prefix/all stale, свой primary key, malformed JSON, rollback cleanup; дальние raw-scan errors сейчас могут предшествовать конфликту           |
| Средний                    | SDK rebuild: не compact-copy старые известные secondary-index stores перед их регенерацией   | Лишние index page writes и последующее retirement                                              | Default compact сохраняет internal stores; reconstructed indexes, unique/TTL, failure до и после active-generation CAS                              |
| Средний                    | `worker.ts::readRawScan`: нормализовать свежие binding rows на месте                         | N дополнительных row objects и один массив                                                     | Собственное владение rows, Uint8Array/partial views/ArrayBuffer/ArrayLike, порядок и независимость повторных результатов                            |
| Низкий                     | `worker.ts::clearRawStoreIfExists`: сохранять positive existence cache после успешного clear | Повторные create_store / StoreExistsError для каждого индекса после clear                      | Clear success/missing/failure, drop, следующая tx, rollback; cache нельзя сохранить после StoreNotFoundError                                        |
| Низкий                     | `engine.rs::maybe_checkpoint`: передать известный WAL len в checkpoint_inner                 | Один `len()` / OPFS getSize на auto-checkpoint                                                 | Eager/deferred commit, explicit/empty checkpoint/close, прежние failpoints и publication barriers                                                   |
| Низкий                     | `has`: TTL header в stack metadata вместо Vec                                                | Одна 16-byte heap allocation на найденный enveloped key                                        | Inline/overflow/expired/missing/raw, malformed header и короткие chunks, staged TTL cleanup                                                         |
| Низкий                     | `worker-server.ts`: объединить два cleanup `.then` одной lane operation                      | Один Promise/callback на lane request                                                          | Порядок одной tx, независимость lanes, продолжение после ошибки, autocommit/exclusive barriers, close при pending storageInfo                       |
| Низкий                     | `MemoryBackend::flush`: раздельные dirty ranges для далеко разнесённых записей               | Копия чистого промежутка между min/max dirty offsets                                           | Существующие durability/overwrite/truncate tests; это test/native memory backend, а не ускорение OPFS                                               |
| Требует отдельного профиля | Ограничить concurrent decompression                                                          | Peak stream/chunk memory; общее число decode и output bytes не уменьшится                      | Mixed/corrupt records, limits/cancellation, ownership до await; concurrency может снизить throughput и изменить первую ошибку                       |
| Требует отдельного профиля | Transport envelopes для уже concurrent scalar requests                                       | Число сообщений, без объединения самостоятельных durable commits                               | IDs, transaction lanes/barriers, timeout/fatal disposal, входы до flush, scalar TTL/error order; sequential await пользы не получит                 |

## Отдельные вопросы корректности

- `worker-client.ts`: баг воспроизведён и исправлен. Malformed packed/error
  response теперь отклоняет Promise, вместо вечного ожидания после удаления
  pending до decode. Таймеры и pending requests проверяются при disposal.
- `create_store_with_compression`: проверить контракт drop → create существовавшего
  store в одной transaction; проверка snapshot catalog может возвращать StoreExists.
- Пропуск хвоста expired overflow при export мог бы сократить чтения, но меняет
  обнаружение corruption. Сначала определить контракт, затем тест; не смешивать
  с оптимизацией копирования или превращать в скрытое ослабление проверки.

## Наблюдаемые результаты прохода VI

| Проверка                                    | До → после                                                                         |
| ------------------------------------------- | ---------------------------------------------------------------------------------- |
| Bounded leaf, 12 rows / limit=1             | Payload/key copies 12 → 1 в обоих направлениях                                     |
| Keys-only pruning, 8 inline values          | Payload allocations 8 → 0                                                          |
| Rewrite, 32 old values / 31 mutations       | Old payload allocations 32 → 1                                                     |
| 65 соседних WAL images                      | Source backend reads 65 → 2                                                        |
| Recovery, 32 commits одной страницы         | Retained transactions/offsets 32/32 → 0/1; pending growths 32 → 1                  |
| Snapshot decode                             | Один owned copy каждого key/store name вместо двух                                 |
| Filtered feed, 4 values по 64 KiB           | Дополнительные owned value copies 4 → 0                                            |
| No-TTL stage, два scan по 1 024 mutations   | TTL examinations 2 048 → 0                                                         |
| Eager commit auto-checkpoint                | WAL len calls 2 → 1                                                                |
| Reconcile, 32 docs / 8 indexes              | Source payload rows и JSON decodes 256 → 32                                        |
| Unique, первый живой conflict               | Physical rows 32 → 1                                                               |
| Clear lifecycle, 8 indexes                  | Create probes 16 → 8                                                               |
| Partial binary argument                     | Переданные backing bytes 65 536 → 8                                                |
| 8 concurrent requests/responses             | Сообщения 8+8 → 1+1                                                                |
| Lane operation                              | Реальные `.then` registrations 3 → 2                                               |
| MemoryBackend, две sparse записи по 4 bytes | Copied bytes 65 492 → 8; shrink/regrowth zero-gap bug исправлен                    |
| Decompression, 16 records                   | Одновременно активны не более 8 decoders; активные операции завершены до rejection |

Point-read проверки также подтверждают отсутствие 16-byte header allocation
при warm has, allocation только returned payload при warm get, один overflow
walk для одинаковых batch keys и один read общего root при cache=1. Compaction
переносит 98 images bounded batches до 64 вместо 98 scalar writes, переиспользуя
concat buffer. Проверены output bytes, reopen и publication failures.

Binding DTO проверяется native serializer oracle: scalar byte elements → 0,
key/value → два `serialize_bytes` вызова; общий JSON shape и CHG1 сохранены.
Raw scan сохраняет принадлежащие binding row objects и массив, нормализуя
значения на месте. Feed limit=0/1 копирует только 0/1 выходных key, продолжая
декодировать хвост и обнаруживать поздние ошибки.

## Совместимость и границы проверки

- Reconcile удерживает не более 4 MiB входных key/value bytes, 4 096 rows и
  одного source store. AST/metadata добавляют память: это не предел JS heap.
  Старые bindings и большие stores сохраняют fresh scan fallback с bounded
  JSON cache и проверкой версии bytes. Scheduler сериализует команды одной
  transaction; fresh native metadata batch сохраняет TTL перед каждым индексом.
  Staged TTL normalization выполняется до traversal, а expired committed keys
  удаляются лишь после успешного batch, как в исходном full scan.
- Первый reconcile scan полностью проверяет source values. Последующие batches
  не перечитывают live overflow tails: I/O ошибки повторного payload read,
  который больше не выполняется, не наблюдаются. Ошибки выполненных reads
  не подавляются.
- Unique lookup теперь сообщает первый установленный conflict без чтения
  ненужного дальнего raw tail. Поздняя corruption за этим conflict больше
  его не опережает; изменение явно закреплено отдельным тестом.
- При нескольких повреждённых compressed records bounded pool может изменить
  первую завершившуюся ошибку и снизить throughput. Сохраняются имя и сообщение
  фактической ошибки; ускорение по времени не заявляется.
- Concurrent transport требует local snapshot clones: восемь операций используют
  16 local и две wire clone операции вместо 16 wire операций. Это сокращение
  сообщений, не числа structural clones. Sequential await сохраняет 8+8 сообщений
  и не добавляет local clones. Batch bounds — 128 commands и примерно 1 MiB;
  один oversized scalar request не разбивается на отдельные команды.
- MemoryBackend хранит O(E) dirty interval endpoints до flush, где E — число
  непересекающихся dirty extents. Это native/test backend. Ancestor cache
  удерживает одну копию 4 KiB на internal level текущего пути, без prefetch
  unrelated leaves; separator checks сохраняются.
- Serialization `StagedStore` не меняется: TTL hint пропускается serde и имеет
  unknown default. Создание через `..Default::default()` сохраняется; полностью
  перечисленные внешние struct literals требуют нового поля.

В финальном лёгком прогоне прошли 104 Rust, 108 SDK portable и 21 OPFS shim
проверка. Большой `profile_remaining_work` исключён; полный Rust workspace,
browser tests и workflows не запускались. Clippy использовал штатный config
с MSRV 1.81 и `-D warnings`; JS lint gates прошли без errors, warnings остаются.

Новые Rust tests входят в обычные Cargo test targets.
`npm run test:minimum-work --workspace @moyodb/sdk` запускает index, read и
transport portable suites без WASM/browser build. Логи before/after находятся
в локальном `node_modules/.cache/moyodb-pass-vi/`.

WASM-target compilation отклонена автоматической проверкой разрешений как
выходящая за текущий лёгкий прогон. Новые WASM wrappers, generated glue и
настоящий OPFS/browser runtime требуют отдельной проверки. Старые артефакты
`packages/sdk/public/engine` не пересобирались; полный workflow status и процент
ускорения по времени на основании этого прохода не заявляются.
