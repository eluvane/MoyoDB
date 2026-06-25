import MoyoDbProofs.Main

def derivedConstantsJson : String := r#"{
  "format_version": 1,
  "superblock_magic": "STKDB001",
  "wal_magic": "WAL1",
  "page_magic": "PAG1",
  "page_size": 4096,
  "superblock_slot_size": 4096,
  "inline_value_limit": 1024,
  "file_kinds": {
    "manifest": 0,
    "main": 1,
    "wal": 2
  },
  "page_kinds": {
    "leaf": 1,
    "internal": 2,
    "overflow": 3
  },
  "value_kinds": {
    "inline": 1,
    "overflow": 2
  },
  "record_tags": {
    "page_image": 1,
    "commit": 2
  },
  "limits": {
    "store_name_bytes": 255,
    "key_bytes": 1024,
    "value_bytes": 8388608
  },
  "failpoints": [
    "after_wal_flush",
    "after_main_flush",
    "before_superblock_flush"
  ]
}
"#

def btreeTracesJson : String := r#"{
  "scenarios": [
    {
      "name": "create_empty_store",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        }
      ],
      "expected": {
        "keys": []
      }
    },
    {
      "name": "insert_one_key",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        }
      ],
      "expected": {
        "keys": [
          "61"
        ],
        "values": {
          "61": "31"
        }
      }
    },
    {
      "name": "overwrite_key",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "32"
        }
      ],
      "expected": {
        "keys": [
          "61"
        ],
        "values": {
          "61": "32"
        }
      }
    },
    {
      "name": "insert_sorted_and_unsorted_keys",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "63",
          "value_hex": "33"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        }
      ],
      "expected": {
        "keys": [
          "61",
          "62",
          "63"
        ],
        "values": {
          "61": "31",
          "62": "32",
          "63": "33"
        }
      }
    },
    {
      "name": "range_scan_with_bounds",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "63",
          "value_hex": "33"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "64",
          "value_hex": "34"
        }
      ],
      "scan": {
        "store": "kv",
        "gte_hex": "62",
        "lt_hex": "64"
      },
      "expected": {
        "keys": [
          "62",
          "63"
        ]
      }
    },
    {
      "name": "delete_key",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        },
        {
          "op": "delete",
          "store": "kv",
          "key_hex": "61"
        }
      ],
      "expected": {
        "keys": [
          "62"
        ],
        "values": {
          "62": "32"
        }
      }
    },
    {
      "name": "delete_missing_key",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "delete",
          "store": "kv",
          "key_hex": "61"
        }
      ],
      "expected": {
        "keys": []
      }
    },
    {
      "name": "root_split_scenario",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "00",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "01",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "02",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "03",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "04",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "05",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "06",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "07",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "08",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "09",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "0a",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "0b",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "0c",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "0d",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "0e",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "0f",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "10",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "11",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "12",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "13",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "14",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "15",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "16",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "17",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "18",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "19",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "1a",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "1b",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "1c",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "1d",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "1e",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "1f",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "20",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "21",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "22",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "23",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "24",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "25",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "26",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "27",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "28",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "29",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "2a",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "2b",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "2c",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "2d",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "2e",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "2f",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "30",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "31",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "32",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "33",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "34",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "35",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "36",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "37",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "38",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "39",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "3a",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "3b",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "3c",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "3d",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "3e",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "3f",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "40",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "41",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "42",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "43",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "44",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "45",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "46",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "47",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "48",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "49",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "4a",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "4b",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "4c",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "4d",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "4e",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "4f",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "50",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "51",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "52",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "53",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "54",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "55",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "56",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "57",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "58",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "59",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "5a",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "5b",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "5c",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "5d",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "5e",
          "value_hex": "01"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "5f",
          "value_hex": "01"
        }
      ],
      "expected": {
        "keys": [
          "00",
          "01",
          "02",
          "03",
          "04",
          "05",
          "06",
          "07",
          "08",
          "09",
          "0a",
          "0b",
          "0c",
          "0d",
          "0e",
          "0f",
          "10",
          "11",
          "12",
          "13",
          "14",
          "15",
          "16",
          "17",
          "18",
          "19",
          "1a",
          "1b",
          "1c",
          "1d",
          "1e",
          "1f",
          "20",
          "21",
          "22",
          "23",
          "24",
          "25",
          "26",
          "27",
          "28",
          "29",
          "2a",
          "2b",
          "2c",
          "2d",
          "2e",
          "2f",
          "30",
          "31",
          "32",
          "33",
          "34",
          "35",
          "36",
          "37",
          "38",
          "39",
          "3a",
          "3b",
          "3c",
          "3d",
          "3e",
          "3f",
          "40",
          "41",
          "42",
          "43",
          "44",
          "45",
          "46",
          "47",
          "48",
          "49",
          "4a",
          "4b",
          "4c",
          "4d",
          "4e",
          "4f",
          "50",
          "51",
          "52",
          "53",
          "54",
          "55",
          "56",
          "57",
          "58",
          "59",
          "5a",
          "5b",
          "5c",
          "5d",
          "5e",
          "5f"
        ]
      }
    },
    {
      "name": "reverse_scan_limit",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "63",
          "value_hex": "33"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "64",
          "value_hex": "34"
        }
      ],
      "scan": {
        "store": "kv",
        "lte_hex": "64",
        "reverse": true,
        "limit": 2
      },
      "expected": {
        "keys": [
          "64",
          "63"
        ],
        "values": {
          "64": "34",
          "63": "33"
        }
      }
    },
    {
      "name": "exclusive_single_key_interval_is_empty",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        }
      ],
      "scan": {
        "store": "kv",
        "gt_hex": "61",
        "lt_hex": "62"
      },
      "expected": {
        "keys": []
      }
    },
    {
      "name": "scan_limit_zero",
      "operations": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "put",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        }
      ],
      "scan": {
        "store": "kv",
        "limit": 0
      },
      "expected": {
        "keys": []
      }
    }
  ]
}
"#

def walRecoveryJson : String := r#"{
  "scenarios": [
    {
      "name": "crash_before_wal_flush",
      "failpoint": null,
      "expect_after": false,
      "note": "tx not even staged to wal in this harness"
    },
    {
      "name": "crash_after_wal_flush_before_main_flush",
      "failpoint": "after_wal_flush",
      "expect_after": true
    },
    {
      "name": "crash_after_main_flush_before_superblock_flush",
      "failpoint": "after_main_flush",
      "expect_after": true
    },
    {
      "name": "crash_before_superblock_flush",
      "failpoint": "before_superblock_flush",
      "expect_after": true
    },
    {
      "name": "clean_commit",
      "failpoint": null,
      "expect_after": true
    },
    {
      "name": "incomplete_wal_tail_ignored",
      "failpoint": "manual_incomplete_tail",
      "expect_after": false
    },
    {
      "name": "commit_page_count_mismatch_ignored",
      "failpoint": "manual_page_count_mismatch",
      "expect_after": false
    }
  ]
}
"#

def txnJson : String := r#"{
  "scenarios": [
    {
      "name": "two_sequential_write_txs",
      "steps": [
        {
          "op": "createStore",
          "store": "kv"
        },
        {
          "op": "write",
          "store": "kv",
          "key_hex": "61",
          "value_hex": "31"
        },
        {
          "op": "write",
          "store": "kv",
          "key_hex": "62",
          "value_hex": "32"
        }
      ],
      "expected": {
        "61": "31",
        "62": "32"
      }
    },
    {
      "name": "readonly_snapshot_before_second_commit",
      "expected": {
        "snapshot_before_second": {
          "61": "31"
        },
        "final": {
          "61": "31",
          "62": "32"
        }
      }
    },
    {
      "name": "later_write_overwrites_key",
      "expected": {
        "first": {
          "61": "31"
        },
        "final": {
          "61": "32"
        }
      }
    }
  ]
}
"#

def writeArtifacts : IO Unit := do
  IO.FS.createDirAll "../artifacts"
  IO.FS.writeFile "../artifacts/derived_constants.json" derivedConstantsJson
  IO.FS.writeFile "../artifacts/btree_traces.json" btreeTracesJson
  IO.FS.writeFile "../artifacts/wal_recovery_traces.json" walRecoveryJson
  IO.FS.writeFile "../artifacts/txn_serialization_traces.json" txnJson

def main : IO Unit := writeArtifacts
