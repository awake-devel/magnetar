// JSDoc mirrors of the server answers the detail, saved, stats, graph and crawl-map screens read
// (android/.../api/Models.kt, StatsModels.kt). Every field may be missing in an older server's
// answer: readers use `?? default`. Types only, no code.

/**
 * @typedef {object} TorrentFile
 * @property {string} [path]
 * @property {number | null} [size]
 */

/**
 * @typedef {object} TrustFactor
 * @property {string} [code]
 * @property {number} [delta]
 * @property {string} [detail]
 * @property {string[]} [evidence]
 */

/**
 * @typedef {object} TrustScore
 * @property {string} [pubkey]
 * @property {number} [score]
 * @property {TrustFactor[]} [factors]
 * @property {string | null} [root]
 * @property {number} [computed_at]
 */

/**
 * @typedef {object} DhtInfo
 * @property {number} [first_seen]
 * @property {number} [fetched_at]
 * @property {string} [title]
 * @property {number} [file_count]
 * @property {number} [total_size]
 * @property {string | null} [category]
 * @property {number} [peers_seen]
 * @property {string} [discovered_via]
 * @property {string | null} [infohash_v2]
 */

/**
 * @typedef {object} Comment
 * @property {string} [event_id]
 * @property {string} [pubkey]
 * @property {number} [created_at]
 * @property {number} [kind]
 * @property {string} [content]
 */

/**
 * @typedef {object} EventRef
 * @property {string} [event_id]
 * @property {string} [pubkey]
 * @property {string} [npub]
 * @property {number} [created_at]
 * @property {string} [title]
 * @property {number} [trust]
 */

/**
 * @typedef {object} Liveness
 * @property {string} [infohash]
 * @property {number | null} [seeders]
 * @property {number | null} [leechers]
 * @property {number | null} [completed]
 * @property {number | null} [dht_peers_est]
 * @property {string} [source]
 * @property {number} [trackers_ok]
 * @property {number} [trackers_failed]
 * @property {number} [checked_at]
 * @property {number} [expires_at]
 */

/**
 * @typedef {object} LivenessPoint
 * @property {number} at
 * @property {number | null} [seeders]
 * @property {number | null} [leechers]
 * @property {number | null} [peers_est]
 */

/**
 * @typedef {object} LivenessHistory
 * @property {string} [infohash]
 * @property {LivenessPoint[]} [points]
 * @property {number} [keep_days]
 */

/**
 * @typedef {object} ProbeTrackDetected
 * @property {string | null} [lang]
 * @property {string[]} [kinds]
 * @property {string | null} [name]
 */

/**
 * @typedef {object} ProbeTrackFix
 * @property {number} [id]
 * @property {string[]} [fields]
 * @property {boolean} [removed]
 * @property {boolean} [added]
 * @property {boolean} [review]
 * @property {ProbeTrackDetected | null} [detected]
 * @property {number} [at]
 * @property {string} [by]
 */

/**
 * @typedef {object} ProbeVideoTrack
 * @property {string} [codec]
 * @property {string | null} [profile]
 * @property {number | null} [width]
 * @property {number | null} [height]
 * @property {number | null} [fps_milli]
 * @property {number | null} [bit_depth]
 * @property {string[]} [hdr]
 * @property {number | null} [dv_profile]
 * @property {number | null} [dv_compat]
 */

/**
 * @typedef {object} ProbeAudioTrack
 * @property {string} [codec]
 * @property {number | null} [channels]
 * @property {string | null} [lang]
 * @property {boolean} [atmos]
 * @property {boolean} [dts_x]
 * @property {string[]} [kinds]
 * @property {string | null} [name]
 * @property {number | null} [pos]
 * @property {ProbeTrackFix | null} [fix]
 */

/**
 * @typedef {object} ProbeSubTrack
 * @property {string} [codec]
 * @property {string | null} [lang]
 * @property {string[]} [kinds]
 * @property {string | null} [name]
 * @property {number | null} [pos]
 * @property {ProbeTrackFix | null} [fix]
 */

/**
 * @typedef {object} ProbeVideo
 * @property {string} [container]
 * @property {number | null} [duration_ms]
 * @property {number | null} [bitrate_kbps]
 * @property {ProbeVideoTrack[]} [video]
 * @property {ProbeAudioTrack[]} [audio]
 * @property {ProbeSubTrack[]} [subs]
 */

/**
 * @typedef {object} ProbeGame
 * @property {string} [platform]
 * @property {string | null} [kind]
 * @property {string | null} [format]
 * @property {string | null} [title_id]
 * @property {string | null} [content_id]
 * @property {string | null} [db_id]
 * @property {string | null} [title]
 * @property {string | null} [version]
 * @property {string | null} [region]
 */

/**
 * @typedef {object} ProbeFileInfo
 * @property {number} [index]
 * @property {number} [dht_index]
 * @property {string} [path]
 * @property {number} [size]
 * @property {string} [label]
 * @property {string} [description]
 * @property {number} [confidence]
 * @property {boolean} [verified]
 * @property {string[]} [flags]
 * @property {ProbeGame | null} [game]
 * @property {ProbeVideo | null} [video]
 */

/**
 * @typedef {object} ProbeUnplacedWas
 * @property {string} [codec]
 * @property {number | null} [channels]
 * @property {string | null} [lang]
 * @property {string[]} [kinds]
 * @property {string | null} [name]
 */

/**
 * @typedef {object} ProbeUnplacedFix
 * @property {number} [id]
 * @property {number} [file]
 * @property {string} [path]
 * @property {string} [track]
 * @property {number | null} [pos]
 * @property {string} [reason]
 * @property {ProbeUnplacedWas | null} [was]
 * @property {string[]} [fields]
 * @property {boolean} [removed]
 * @property {string | null} [lang]
 * @property {string[]} [kinds]
 * @property {string | null} [name]
 * @property {number} [at]
 */

/**
 * @typedef {object} ProbeInfo
 * @property {number} [version]
 * @property {number} [probed_at]
 * @property {string} [status]
 * @property {boolean} [verified]
 * @property {ProbeFileInfo[]} [files]
 * @property {ProbeUnplacedFix[]} [unplaced]
 */

/**
 * One correction (D130). Fields left out of the JSON are absent here too.
 * @typedef {object} ProbeCorrectionItem
 * @property {number} [id]
 * @property {number} file
 * @property {string} track
 * @property {number} [pos]
 * @property {string[]} set
 * @property {string} [lang]
 * @property {string[]} kinds
 * @property {string} [name]
 * @property {boolean} removed
 */

/**
 * `GET /api/v1/torrents/{ih}`.
 * @typedef {object} TorrentDetail
 * @property {import("./types.js").TorrentSummary & {event_id?: string, pow_bits?: number}} summary
 * @property {string} [description]
 * @property {TorrentFile[]} [files]
 * @property {string[]} [trackers]
 * @property {string[]} [hashtags]
 * @property {string[]} [category_path]
 * @property {string[][]} [external_ids]
 * @property {string} [magnet]
 * @property {Liveness | null} [liveness]
 * @property {TrustScore} [trust]
 * @property {EventRef[]} [events]
 * @property {Comment[]} [comments]
 * @property {import("./types.js").MediaInfo | null} [media]
 * @property {DhtInfo | null} [dht]
 * @property {ProbeInfo | null} [probe]
 */

/**
 * @typedef {object} FacetCount
 * @property {string} value
 * @property {number} count
 */

/**
 * @typedef {object} Breakdown
 * @property {string} field
 * @property {number} [total]
 * @property {FacetCount[]} [counts]
 * @property {number} [other]
 * @property {number} [other_values]
 * @property {number} [missing]
 */

/**
 * @typedef {object} BreakdownSet
 * @property {number} [total]
 * @property {Breakdown[]} [breakdowns]
 * @property {number} [computed_at]
 */

/**
 * @typedef {object} TimeseriesPoint
 * @property {number} at
 * @property {number} value
 */

/**
 * @typedef {object} Timeseries
 * @property {string} metric
 * @property {string} [kind]
 * @property {string} [bucket]
 * @property {TimeseriesPoint[]} [points]
 */

/**
 * @typedef {object} BatchReport
 * @property {number} [hashes]
 * @property {number} [ms]
 */

/**
 * @typedef {object} LivenessStats
 * @property {boolean} [enabled]
 * @property {number} [torrents]
 * @property {number} [never_checked]
 * @property {number} [due_now]
 * @property {number} [checked_24h]
 * @property {number} [checks_per_hour]
 * @property {number} [alive]
 * @property {number} [dead]
 * @property {number} [unknown]
 * @property {number} [trackers]
 * @property {number} [trackers_backed_off]
 * @property {BatchReport | null} [last_batch]
 * @property {number | null} [last_batch_at]
 */

/**
 * @typedef {object} SearchIndexStatus
 * @property {string} [state]
 * @property {number} [docs]
 * @property {number} [build_done]
 * @property {number} [build_total]
 * @property {number | null} [last_commit]
 * @property {number} [pending]
 * @property {number | null} [lag_secs]
 * @property {number} [bytes]
 * @property {string | null} [error]
 */

/**
 * @typedef {object} UpnpMapping
 * @property {string} [protocol]
 * @property {number} [port]
 */

/**
 * @typedef {object} UpnpStatus
 * @property {string} [state]
 * @property {UpnpMapping[]} [mappings]
 * @property {string | null} [last_error]
 */

/**
 * @typedef {object} DhtStats
 * @property {string} [mode]
 * @property {boolean} [running]
 * @property {number} [uptime_secs]
 * @property {number} [virtual_nodes]
 * @property {string} [external_ip]
 * @property {number} [queries_per_sec]
 * @property {number} [answered_share]
 * @property {boolean} [brake]
 * @property {number} [incoming_per_min]
 * @property {number} [hashes_per_min]
 * @property {number} [queue_len]
 * @property {boolean} [sampling_paused]
 * @property {number} [metadata_per_min]
 * @property {number} [fetch_success_rate]
 * @property {number} [stored_total]
 * @property {number} [adult_total]
 * @property {number} [blocked_total]
 * @property {UpnpStatus | null} [upnp]
 * @property {string | null} [last_error]
 */

/**
 * @typedef {object} CrawlReport
 * @property {number} [relays_tried]
 * @property {number} [relays_ok]
 * @property {number} [inserted]
 */

/**
 * `GET /api/v1/stats/health`.
 * @typedef {object} StatsHealth
 * @property {number} [torrents]
 * @property {LivenessStats | null} [liveness]
 * @property {SearchIndexStatus | null} [search_index]
 * @property {{total?: number, enabled?: number, ok_24h?: number, failing?: number, productive?: number}} [relays]
 * @property {{running?: boolean, last_run?: number | null, last_report?: CrawlReport | null, passes?: number, last_error?: string | null}} [crawler]
 * @property {number} [media_linked]
 * @property {number} [attrs_missing]
 * @property {number} [computed_at]
 * @property {DhtStats | null} [dht]
 */

/**
 * @typedef {object} RouteCount
 * @property {string} via
 * @property {number} [seen]
 * @property {number} [stored]
 */

/**
 * @typedef {object} RouteHour
 * @property {number} bucket
 * @property {Record<string, number>} [seen]
 * @property {Record<string, number>} [stored]
 */

/**
 * @typedef {object} RelaySourceCount
 * @property {string} [source]
 * @property {number} [relays]
 * @property {number} [enabled]
 * @property {number} [delivered]
 * @property {number} [events]
 * @property {number} [torrents]
 */

/**
 * @typedef {object} TopRelay
 * @property {string} [url]
 * @property {string} [source]
 * @property {boolean} [enabled]
 * @property {boolean} [ok_24h]
 * @property {number} [events]
 * @property {number} [torrents]
 * @property {string | null} [country]
 * @property {number | null} [asn]
 * @property {string | null} [as_org]
 */

/**
 * @typedef {object} CrawlMachine
 * @property {string} [name]
 * @property {string} [kind]
 * @property {boolean} [connected]
 * @property {number} [dht_triaged]
 * @property {number} [dht_queued]
 * @property {number} [dht_metadata]
 * @property {number} [dht_stored]
 * @property {number} [seed_checks]
 * @property {number} [stored_per_min]
 */

/**
 * `GET /api/v1/stats/sources`.
 * @typedef {object} CrawlSources
 * @property {{routes?: RouteCount[], hourly?: RouteHour[], hourly_since?: number} | null} [dht]
 * @property {{by_source?: RelaySourceCount[], top_relays?: TopRelay[], torrents_since?: number} | null} [nostr]
 * @property {CrawlMachine[]} [machines]
 * @property {number} [computed_at]
 */

/**
 * A country (`asn` null) or a network; `counts` parallel to `CrawlMap.metrics`.
 * @typedef {object} GeoCount
 * @property {string | null} [country]
 * @property {number | null} [asn]
 * @property {string | null} [as_org]
 * @property {number[]} [counts]
 */

/**
 * `GET /api/v1/stats/map`.
 * @typedef {object} CrawlMap
 * @property {string} [layer]
 * @property {string} [window]
 * @property {boolean} [available]
 * @property {string | null} [reason]
 * @property {{country_build?: number | null}} [database]
 * @property {string[]} [metrics]
 * @property {GeoCount[]} [countries]
 * @property {GeoCount[]} [networks]
 * @property {number[]} [unknown]
 * @property {number[]} [totals]
 * @property {number} [since]
 * @property {number} [computed_at]
 */

/**
 * A `(name, count)` pair as the server sends it (`["adult", 12]`).
 * @typedef {unknown} NamedPair
 */

/**
 * @typedef {object} ProbeBackfillStats
 * @property {number} [waiting]
 * @property {number} [leased]
 * @property {number} [retrying]
 * @property {number} [remaining]
 * @property {NamedPair[]} [closed]
 * @property {number} [probed]
 * @property {boolean} [filling]
 * @property {number | null} [filled_at]
 * @property {number} [at]
 */

/**
 * @typedef {object} WorkerBackfillState
 * @property {number} [outstanding]
 * @property {number} [no_peers]
 * @property {number} [failed]
 * @property {number} [fetched]
 * @property {number} [lease_errors]
 */

/**
 * @typedef {object} WorkerStats
 * @property {string} [name]
 * @property {boolean} [connected]
 * @property {string | null} [version]
 * @property {number} [backfill_leased_total]
 * @property {number} [backfill_probed_total]
 * @property {number} [backfill_unprobed_total]
 * @property {{discover?: boolean | null, backfill?: WorkerBackfillState | null, probe?: object | null} | null} [dht]
 */

/**
 * @typedef {object} ProbeKindAccuracy
 * @property {string} [track]
 * @property {string} [kind]
 * @property {string} [source]
 * @property {number} [checked]
 * @property {number} [wrong]
 */

/**
 * @typedef {object} ProbeCorrectionStats
 * @property {number} [tracks]
 * @property {number} [checked]
 * @property {number} [corrected]
 * @property {number} [removed]
 * @property {number} [added]
 * @property {{checked?: number, wrong?: number, filled?: number}} [languages]
 * @property {ProbeKindAccuracy[]} [kinds]
 * @property {{track?: string, kind?: string, count?: number}[]} [missed]
 */

/**
 * `GET /api/v1/stats` (the parts the web reads).
 * @typedef {object} FleetStats
 * @property {WorkerStats[]} [workers]
 * @property {ProbeBackfillStats | null} [probe_backfill]
 * @property {ProbeCorrectionStats | null} [probe_corrections]
 */

/**
 * @typedef {object} GraphNode
 * @property {string} [id]
 * @property {string} [kind]
 * @property {string} [label]
 * @property {number} [weight]
 * @property {number | null} [score]
 * @property {string[]} [detail]
 */

/**
 * @typedef {object} GraphEdge
 * @property {number} source
 * @property {number} target
 * @property {number} [weight]
 * @property {string} [kind]
 */

/**
 * `GET /api/v1/graphs/…`.
 * @typedef {object} Graph
 * @property {string} [kind]
 * @property {GraphNode[]} [nodes]
 * @property {GraphEdge[]} [edges]
 * @property {boolean} [truncated]
 * @property {string | null} [note]
 */

export {};
