"""Regression tests for check_fence_tags.py.

Run with: python3 -m unittest discover -s .github/scripts -p 'test_*.py'
"""

import json
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

import check_fence_tags as c

SCHEMAS = c.load_schemas()


def check(markdown):
    """Run the check on a Markdown string. Returns (blocks, failures)."""
    blocks, failures = [], []
    c.check_text(textwrap.dedent(markdown).lstrip("\n"), "t.md", blocks, failures, SCHEMAS)
    return blocks, failures


class QuotedFences(unittest.TestCase):
    def test_untagged_block_in_quote_fails(self):
        _, failures = check("""
            > Note:
            >
            > ```json
            > { "target": "pod/api" }
            > ```
        """)
        self.assertEqual(failures, ["t.md:3: untagged json block"])

    def test_malformed_config_in_quote_fails(self):
        _, failures = check("""
            > ```json mirrord
            > { "target": "pod/api", }
            > ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("t.md:1: 'mirrord' block does not parse as json"))

    def test_quote_markers_stripped_from_body(self):
        blocks, failures = check("""
            > ```yaml mirrord-up=services.*
            > target:
            >   path: deployment/api
            > ```
        """)
        self.assertEqual(failures, [])
        self.assertEqual(blocks, [("t.md", 1, "mirrord-up=services.*", "target")])

    def test_nested_quote(self):
        _, failures = check("""
            > > ```yaml
            > > a: 1
            > > ```
        """)
        self.assertEqual(failures, ["t.md:1: untagged yaml block"])

    def test_block_ends_with_its_quote(self):
        # The quoted block is never closed; the fence after the quote is its own block.
        blocks, failures = check("""
            > ```json other
            > {}

            ```json
            {}
            ```
        """)
        self.assertEqual([b[1] for b in blocks], [1, 4])
        self.assertEqual(failures, ["t.md:4: untagged json block"])


class Fences(unittest.TestCase):
    def test_list_indented_fence(self):
        blocks, failures = check("""
            - item

              ```json mirrord
              { "target": "pod/api" }
              ```
        """)
        self.assertEqual(failures, [])
        self.assertEqual(blocks, [("t.md", 3, "mirrord", "target")])

    def test_other_languages_ignored(self):
        blocks, _ = check("""
            ```bash
            echo hi
            ```
        """)
        self.assertEqual(blocks, [])

    def test_unknown_tag_fails(self):
        _, failures = check("""
            ```yaml bogus
            a: 1
            ```
        """)
        self.assertEqual(failures, ["t.md:1: unknown tag 'bogus'"])


class Indentation(unittest.TestCase):
    def test_four_space_fence_is_indented_code(self):
        blocks, failures = check("""
            Example of a literal fence:

                ```json
                {}
                ```
        """)
        self.assertEqual((blocks, failures), ([], []))

    def test_four_space_backticks_do_not_close_block(self):
        # The indented ``` is content of the yaml block, so the block runs to the
        # unindented fence, keeps the trailing key, and nothing after it opens a block.
        blocks, failures = check("""
            ```yaml other
            literal: |
                ```
            after: 1
            ```

            Done.
        """)
        self.assertEqual(failures, [])
        self.assertEqual(blocks, [("t.md", 1, "other", "literal,after")])

    def test_four_space_backticks_stay_in_config_block(self):
        # If the indented ``` closed the block, "bogus" would escape the root key check.
        _, failures = check("""
            ```yaml mirrord-up=services.*
            run:
              command:
                - |
                    ```
            bogus: 1
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("keys bogus are not properties of 'services.*'", failures[0])

    def test_closing_fence_inside_list_item(self):
        # 4 spaces past the item's content indent is a literal line; 0 closes the block.
        blocks, failures = check("""
            - item

              ```json other
              { "target": "pod/api" }
                  ```
              ```

            ```json
            {}
            ```
        """)
        self.assertEqual([b[1] for b in blocks], [3, 8])
        self.assertEqual(failures, ["t.md:8: untagged json block"])

    def test_fence_too_deep_inside_list_item_is_indented_code(self):
        blocks, _ = check("""
            - item

                  ```json
                  {}
                  ```
        """)
        self.assertEqual(blocks, [])

    def test_nested_list_fence(self):
        blocks, failures = check("""
            - outer
              - inner

                ```json
                {}
                ```
        """)
        self.assertEqual(failures, ["t.md:4: untagged json block"])

    def test_fence_on_list_marker_line(self):
        _, failures = check("""
            1. ```yaml
               a: 1
               ```
        """)
        self.assertEqual(failures, ["t.md:1: untagged yaml block"])

    def test_three_space_fence_counts(self):
        _, failures = check("""
            Text.

               ```json
               {}
               ```
        """)
        self.assertEqual(failures, ["t.md:3: untagged json block"])


class Containers(unittest.TestCase):
    def test_unclosed_list_fence_ends_with_its_item(self):
        blocks, failures = check("""
            - item

              ```json other
              {}

            ```yaml
            a: 1
            ```
        """)
        self.assertEqual([b[1] for b in blocks], [3, 6])
        self.assertEqual(failures, ["t.md:6: untagged yaml block"])

    def test_html_comment_hides_fence(self):
        blocks, failures = check("""
            <!--
            ```json
            {}
            ```
            -->

            ```json
            {}
            ```
        """)
        self.assertEqual(failures, ["t.md:7: untagged json block"])
        self.assertEqual([b[1] for b in blocks], [7])

    def test_single_line_html_comment_does_not_hide_next_fence(self):
        _, failures = check("""
            <!-- note -->
            ```json
            {}
            ```
        """)
        self.assertEqual(failures, ["t.md:2: untagged json block"])

    def test_html_block_ends_at_blank_line(self):
        # The fence directly under <div> is raw HTML; after the blank line it is Markdown.
        blocks, failures = check("""
            <div>
            ```json
            {}
            ```

            ```json
            {}
            ```
            </div>
        """)
        self.assertEqual(failures, ["t.md:6: untagged json block"])

    def test_pre_block_hides_fence(self):
        blocks, _ = check("""
            <pre>

            ```json
            {}
            ```

            </pre>
        """)
        self.assertEqual(blocks, [])


class ConfigBlocks(unittest.TestCase):
    def test_placeholder_value_fails(self):
        _, failures = check("""
            ```json mirrord
            { "feature": { "db_branches": [ { "connection": { "url": "..." } } ] } }
            ```
        """)
        self.assertEqual(failures, ["t.md:1: 'mirrord' block contains a '...' placeholder"])

    def test_yaml_placeholder_fails(self):
        _, failures = check("""
            ```yaml mirrord-up
            services: ...
            ```
        """)
        self.assertEqual(failures, ["t.md:1: 'mirrord-up' block contains a '...' placeholder"])

    def test_yaml_document_end_line_fails(self):
        _, failures = check("""
            ```yaml mirrord-up
            services:
              api:
                run:
                  command: ["node", "app.js"]
            ...
            ```
        """)
        self.assertEqual(failures, ["t.md:6: 'mirrord-up' block contains a '...' placeholder line"])

    def test_yaml_placeholder_list_item_fails(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*
            run:
              command:
                - ...
            ```
        """)
        self.assertEqual(failures, ["t.md:1: 'mirrord-up=services.*' block contains a '...' placeholder"])

    def test_json_line_comment_fails(self):
        _, failures = check("""
            ```json mirrord
            {
              // steal traffic
              "target": "pod/api"
            }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("t.md:1: 'mirrord' block does not parse as json"))

    def test_quoted_templates_pass(self):
        _, failures = check("""
            ```json mirrord
            { "feature": { "network": { "incoming": { "mode": "steal", "http_filter": { "header_filter": "^baggage: .*mirrord-session={{ key }}.*$" } } } } }
            ```

            ```yaml mirrord-up
            services:
              api:
                target:
                  namespace: "{{ get_env(name='DEV_NAMESPACE', default='default') }}"
                run:
                  command: ["node", "app.js"]
            ```
        """)
        self.assertEqual(failures, [])

    def test_unquoted_yaml_template_fails(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*
            target:
              namespace: {{ key }}
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith("t.md:1: 'mirrord-up=services.*' block does not parse as yaml"))

    def test_json_duplicate_key_fails(self):
        _, failures = check("""
            ```json mirrord
            { "target": "pod/api", "target": "pod/web" }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("duplicate key(s) target", failures[0])

    def test_yaml_duplicate_key_fails(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*
            target:
              path: deployment/api
            target:
              path: deployment/web
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("duplicate key 'target'", failures[0])

    def test_same_key_in_different_objects_passes(self):
        _, failures = check("""
            ```json mirrord
            { "feature": { "fs": { "mode": "read" }, "network": { "incoming": { "mode": "steal" } } } }
            ```
        """)
        self.assertEqual(failures, [])

    def test_json_number_overflow_fails(self):
        _, failures = check("""
            ```json mirrord
            { "agent": { "dns": { "timeout": 1e999 } } }
            ```
        """)
        self.assertEqual(failures, ["t.md:1: 'mirrord' block contains a NaN or infinite number"])

    def test_yaml_inf_and_nan_fail(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*.config_patch
            agent:
              dns:
                timeout: .inf
            ```

            ```yaml mirrord-up=services.*.config_patch
            agent:
              dns:
                attempts: .nan
            ```
        """)
        self.assertEqual(failures, [
            "t.md:1: 'mirrord-up=services.*.config_patch' block contains a NaN or infinite number",
            "t.md:7: 'mirrord-up=services.*.config_patch' block contains a NaN or infinite number",
        ])

    def test_finite_floats_pass(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*.config_patch
            experimental:
              latency:
                receive_delay: 1.5
            ```
        """)
        self.assertEqual(failures, [])

    def test_shared_aliases_checked_once(self):
        # Each anchor references the previous one twice: 2**40 paths, one shared subtree.
        lines = ["a0: &a0 [x]"] + [f"a{i}: &a{i} [*a{i - 1}, *a{i - 1}]" for i in range(1, 41)]
        body = "\n".join(lines)
        blocks, failures = [], []
        start = time.monotonic()
        c.check_text(f"```yaml mirrord-up=services.*.config_patch\n{body}\n```\n", "t.md", blocks, failures, SCHEMAS)
        self.assertLess(time.monotonic() - start, 2)
        self.assertEqual(failures, [])

    def test_placeholder_under_shared_alias_still_found(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*.config_patch
            base: &base [x, "..."]
            one: [*base, *base]
            two: [*base]
            ```
        """)
        self.assertEqual(failures, ["t.md:1: 'mirrord-up=services.*.config_patch' block contains a '...' placeholder"])

    def test_recursive_alias_reported_and_scan_continues(self):
        _, failures = check("""
            ```yaml mirrord-up
            services: &loop
              api: *loop
            ```

            ```json
            {}
            ```
        """)
        self.assertEqual(failures, [
            "t.md:1: 'mirrord-up' block contains a recursive YAML alias",
            "t.md:6: untagged json block",
        ])

    def test_nan_fails(self):
        _, failures = check("""
            ```json mirrord
            { "agent": { "dns": { "timeout": NaN } } }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("non-standard JSON constant NaN", failures[0])

    def test_yaml_comments_allowed(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*
            # run it locally
            run:
              command: ["node", "app.js"]
            ```
        """)
        self.assertEqual(failures, [])

    def test_unresolved_path_fails(self):
        _, failures = check("""
            ```json mirrord=feature.db_branch[]
            { "copy": { "mode": "schema" } }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("path 'feature.db_branch[]' does not resolve", failures[0])

    def test_unknown_root_key_fails(self):
        _, failures = check("""
            ```json mirrord=feature.db_branches[]
            { "bogus": true }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("keys bogus are not properties of 'feature.db_branches[]'", failures[0])

    def test_keys_from_exclusive_alternatives_fail(self):
        # Flyway and Liquibase are oneOf branches; changelog_file is Liquibase only.
        _, failures = check("""
            ```json mirrord=feature.db_branches[].migrations
            { "flavor": "flyway", "path": "./migrations", "changelog_file": "db.changelog-master.xml" }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("do not match any one alternative of 'feature.db_branches[].migrations'", failures[0])

    def test_keys_from_two_alternatives_fail(self):
        # locations is Flyway only, changelog_file is Liquibase only.
        _, failures = check("""
            ```json mirrord=feature.db_branches[].migrations
            { "locations": ["filesystem:/flyway/sql"], "changelog_file": "db.changelog-master.xml" }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("do not match any one alternative", failures[0])

    def test_discriminator_mismatch_fails(self):
        _, failures = check("""
            ```json mirrord=feature.db_branches[]
            { "type": "redis", "migrations": { "flavor": "flyway", "path": "./migrations" } }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("do not match any one alternative of 'feature.db_branches[]'", failures[0])

    def test_keys_from_one_alternative_pass(self):
        _, failures = check("""
            ```json mirrord=feature.db_branches[].migrations
            { "flavor": "flyway", "path": "./migrations", "image": "flyway/flyway:12" }
            ```

            ```json mirrord=feature.db_branches[].migrations
            { "flavor": "liquibase", "path": "./changelog", "changelog_file": "db.changelog-master.xml" }
            ```
        """)
        self.assertEqual(failures, [])

    def test_unconstrained_schema_accepts_any_keys(self):
        # config_patch is declared with only a description, so any object fits.
        _, failures = check("""
            ```yaml mirrord-up=services.*.config_patch
            feature:
              split_queues:
                "*":
                  queue_type: SQS
            ```

            ```json mirrord-up=services.*.config_patch.feature.split_queues
            { "my-queue": { "queue_type": "SQS" } }
            ```
        """)
        self.assertEqual(failures, [])

    def test_path_next_to_unconstrained_schema_still_checked(self):
        _, failures = check("""
            ```yaml mirrord-up=services.*.config_pach
            feature: {}
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertIn("path 'services.*.config_pach' does not resolve", failures[0])

    def test_valid_fragments_pass(self):
        _, failures = check("""
            ```json mirrord=feature.db_branches[]
            { "connection": { "url": "DATABASE_URL" }, "copy": { "mode": "schema" } }
            ```

            ```yaml mirrord-up=services.*
            run:
              command: ["node", "app.js"]
            ```
        """)
        self.assertEqual(failures, [])


class SchemaValidation(unittest.TestCase):
    def test_unknown_key_in_full_block_fails_with_path(self):
        _, failures = check("""
            ```json mirrord
            { "target": { "path": "pod/my-pod", "container": "app" } }
            ```
        """)
        self.assertEqual(failures, [
            "t.md:1: 'mirrord' block fails skills/mirrord-config/references/schema.json at target: "
            "Additional properties are not allowed ('container' was unexpected)",
        ])

    def test_ambiguous_oneof_names_the_branches(self):
        # A schema of its own, deliberately ambiguous, so upstream schema fixes can't break it.
        branch = {"type": "object", "properties": {"url": {"type": "string"}, "location": {"type": "string"}}}
        schema = {
            "$defs": {
                "LocalBranch": {**branch, "properties": {**branch["properties"], "location": {"const": "local"}}},
                "RemoteBranch": {**branch, "properties": {**branch["properties"], "location": {"const": "remote"}}},
            },
            "type": "object",
            "properties": {"branch": {"oneOf": [{"$ref": "#/$defs/LocalBranch"}, {"$ref": "#/$defs/RemoteBranch"}]}},
        }
        blocks, failures = [], []
        c.check_text('```json mirrord\n{ "branch": { "url": "REDIS_URL" } }\n```\n', "t.md", blocks, failures,
                     {**SCHEMAS, "mirrord": schema}, {**c.SCHEMAS, "mirrord": "ambiguous.json"})
        self.assertEqual(failures, [
            "t.md:1: 'mirrord' block fails ambiguous.json at branch: matches more than one of LocalBranch, RemoteBranch",
        ])

    def test_valid_full_block_passes(self):
        _, failures = check("""
            ```json mirrord
            { "target": { "path": "pod/my-pod/container/app" }, "feature": { "network": { "incoming": "steal" } } }
            ```
        """)
        self.assertEqual(failures, [])

    def test_fragment_without_required_siblings_passes(self):
        _, failures = check("""
            ```json mirrord=feature.db_branches[]
            { "copy": { "mode": "schema" } }
            ```

            ```yaml mirrord-up=services.*
            target:
              path: deployment/api
            ```
        """)
        self.assertEqual(failures, [])

    def test_fragment_value_fails_with_path(self):
        _, failures = check("""
            ```json mirrord=feature.network
            { "incoming": { "mode": "stael" } }
            ```
        """)
        self.assertEqual(len(failures), 1)
        self.assertTrue(failures[0].startswith(
            "t.md:1: 'mirrord=feature.network' fragment fails skills/mirrord-config/references/schema.json at incoming"),
            failures[0])

    def test_schema_from_another_file(self):
        schema = {"type": "object", "properties": {"target": {"type": "string"}}, "additionalProperties": False}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "generated.json"
            path.write_text(json.dumps(schema))
            schemas = {**SCHEMAS, **c.load_schemas({"mirrord": path})}
        blocks, failures = [], []
        c.check_text('```json mirrord\n{ "target": 1 }\n```\n', "t.md", blocks, failures, schemas,
                     {**c.SCHEMAS, "mirrord": "generated.json"})
        self.assertEqual(failures, ["t.md:1: 'mirrord' block fails generated.json at target: 1 is not of type 'string'"])


if __name__ == "__main__":
    unittest.main()
