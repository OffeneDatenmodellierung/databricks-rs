import json
import tempfile
import unittest
from pathlib import Path

import check_parity

API_GO = """package jobs

type JobsInterface interface {
\tGet(ctx context.Context, request GetJobRequest) (*Job, error)
\tGetByJobId(ctx context.Context, jobId int64) (*Job, error)
\tGetBySettingsName(ctx context.Context, name string) (*BaseJob, error)
\tListAll(ctx context.Context, request ListJobsRequest) ([]BaseJob, error)
\tWaitGetRunDone(ctx context.Context, runId int64) (*Run, error)
\tRunNowAndWait(ctx context.Context, request RunNow) (*Run, error)
\tSub() SubInterface
\tBaseJobSettingsNameToJobIdMap(ctx context.Context, request ListJobsRequest) (map[string]int64, error)
}

func (a *JobsAPI) GetBySettingsName(ctx context.Context, name string) (*BaseJob, error) {
\tresult, err := a.jobsImpl.ListAll(ctx, ListJobsRequest{})
\treturn nil, err
}

func (a *JobsAPI) Create(ctx context.Context, request CreateRequest) (CreateOperationInterface, error) {
\treturn nil, nil
}
"""

EXT_GO = """package jobs

func (a *JobsAPI) GetRun(ctx context.Context, request GetRunRequest) (*Run, error) { return nil, nil }
func (e *expandedIterator) Next(ctx context.Context) (BaseJob, error) { return BaseJob{}, nil }
func Helper() {}
"""


def ir(lro: bool) -> dict:
    methods = [{"name": "Get"}, {"name": "List"}, {"name": "RunNow"}]
    if lro:
        methods.append({"name": "Create", "lro": {"poll": "GetOperation"}})
    return {
        "source": {"go_sdk_version": "v0.0.1"},
        "services": [
            {"client": "workspace", "accessor": "Jobs", "package": "jobs", "name": "Jobs", "methods": methods}
        ],
    }


PARITY = """
[accessors]
"workspace.Users" = { status = "gap", issue = 14 }

[methods]
"*.*.*To*Map" = { status = "gap", issue = 17 }
"*.*.GetBy*" = { status = "gap", issue = 17 }

[helpers]
"jobs.JobsAPI.GetRun" = { status = "implemented", rust = "src/ext.rs#get_run" }
"jobs.Helper" = { status = "na", reason = "test" }
"WorkspaceClient.CurrentWorkspaceID" = { status = "gap", issue = 14 }
"""


class Parity(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.go, self.root = base / "go", base / "root"
        (self.go / "service/jobs").mkdir(parents=True)
        (self.go / "service/jobs/api.go").write_text(API_GO)
        (self.go / "service/jobs/ext_api.go").write_text(EXT_GO)
        (self.go / "service/jobs/ext_api_test.go").write_text("func TestX() {}\n")
        (self.go / "workspace_client.go").write_text(
            "type WorkspaceClient struct {\n\tJobs jobs.JobsInterface\n\tUsers iam.UsersInterface\n}\n"
        )
        (self.go / "account_client.go").write_text("type AccountClient struct {\n}\n")
        (self.go / "workspace_functions.go").write_text(
            "func (w *WorkspaceClient) CurrentWorkspaceID(ctx context.Context) (int64, error) {}\n"
        )
        (self.root / "spec").mkdir(parents=True)
        (self.root / "codegen").mkdir()
        (self.root / "src").mkdir()
        (self.root / "src/ext.rs").write_text("pub async fn get_run() {}\n")
        self.write(ir(lro=True), PARITY)

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, ir_doc: dict, parity: str) -> None:
        (self.root / "spec/ir.json").write_text(json.dumps(ir_doc))
        (self.root / "codegen/parity.toml").write_text(parity)

    def run_check(self):
        return check_parity.check(self.go, self.root)

    def test_everything_accounted_for(self):
        r = self.run_check()
        self.assertEqual(r.errors, [])
        self.assertEqual(r.covered["by"], ["jobs.Jobs.GetByJobId"])
        self.assertEqual(r.covered["all"], ["jobs.Jobs.ListAll"])
        self.assertEqual(r.covered["wait"], ["jobs.Jobs.WaitGetRunDone"])
        self.assertEqual(r.covered["and_wait"], ["jobs.Jobs.RunNowAndWait"])
        self.assertEqual(r.covered["sub_accessor"], ["jobs.Jobs.Sub"])
        # A list-and-search GetBy is not a positional shortcut.
        self.assertEqual(r.listed["jobs.Jobs.GetBySettingsName"][0], "*.*.GetBy*")
        md = check_parity.markdown(r, "v0.0.1")
        self.assertIn("| `*.*.GetBy*` (1) | #17 |", md)
        self.assertIn("`src/ext.rs#get_run`", md)
        self.assertEqual(check_parity.main([str(self.go), "--root", str(self.root)]), 0)
        self.assertTrue((self.root / "spec/PARITY.md").exists())

    def test_unlisted_surface_and_stale_entries_fail(self):
        parity = PARITY.replace('"jobs.Helper" = { status = "na", reason = "test" }\n', "")
        parity += '"jobs.Gone" = { status = "na", reason = "x" }\n'
        self.write(ir(lro=True), parity)
        errors = "\n".join(self.run_check().errors)
        self.assertIn("helper jobs.Helper: not in parity.toml", errors)
        self.assertIn("[helpers] jobs.Gone: matches nothing in Go (stale)", errors)
        self.assertEqual(check_parity.main([str(self.go), "--root", str(self.root)]), 1)

    def test_missing_lro_and_accessor_fail(self):
        self.write(ir(lro=False), PARITY.replace('"workspace.Users" = { status = "gap", issue = 14 }', ""))
        errors = "\n".join(self.run_check().errors)
        self.assertIn("long-running jobs.Jobs.Create", errors)
        self.assertIn("accessor workspace.Users", errors)

    def test_entries_are_validated(self):
        bad = PARITY.replace("src/ext.rs#get_run", "src/ext.rs#nope").replace(
            'issue = 17 }\n"*.*.GetBy*"', 'issue = "x" }\n"*.*.GetBy*"'
        )
        bad += '"jobs.JobsAPI.Other" = { status = "maybe" }\n"jobs.*" = { status = "na" }\n'
        self.write(ir(lro=True), bad)
        errors = "\n".join(self.run_check().errors)
        self.assertIn("`fn nope` not found", errors)
        self.assertIn("gap needs an issue number", errors)
        self.assertIn("unknown status 'maybe'", errors)
        self.assertIn("na needs a reason", errors)


class Lookup(unittest.TestCase):
    def test_the_most_specific_pattern_wins(self):
        entries = {"*.*.GetBy*": {"issue": 17}, "iam.*.GetBy*": {"issue": 14}, "iam.Users.GetByName": {"issue": 1}}
        self.assertEqual(check_parity.lookup(entries, "iam.Users.GetByName")[0], "iam.Users.GetByName")
        self.assertEqual(check_parity.lookup(entries, "iam.Groups.GetByName")[0], "iam.*.GetBy*")
        self.assertEqual(check_parity.lookup(entries, "jobs.Jobs.GetByName")[0], "*.*.GetBy*")
        self.assertIsNone(check_parity.lookup(entries, "jobs.Jobs.List"))


if __name__ == "__main__":
    unittest.main()
