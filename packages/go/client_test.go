package polyglot

import (
	"encoding/json"
	"errors"
	"os"
	"reflect"
	"runtime"
	"strings"
	"testing"
)

func TestPublicAPIMatchesCapabilityContract(t *testing.T) {
	path := os.Getenv("POLYGLOT_API_CONTRACT")
	if path == "" {
		t.Skip("POLYGLOT_API_CONTRACT is not set")
	}

	payload, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read API capability contract: %v", err)
	}
	var contract struct {
		SchemaVersion int      `json:"schemaVersion"`
		Layers        []string `json:"layers"`
		Capabilities  []struct {
			ID     string `json:"id"`
			Layers map[string]struct {
				Status  string   `json:"status"`
				Symbols []string `json:"symbols"`
				Notes   string   `json:"notes"`
			} `json:"layers"`
		} `json:"capabilities"`
	}
	if err := json.Unmarshal(payload, &contract); err != nil {
		t.Fatalf("parse API capability contract: %v", err)
	}
	if contract.SchemaVersion != 1 {
		t.Fatalf("schemaVersion = %d, want 1", contract.SchemaVersion)
	}

	clientType := reflect.TypeOf(&Client{})
	packageSymbols := map[string]any{
		"MergeInto": MergeInto,
	}
	seen := make(map[string]bool, len(contract.Capabilities))
	for _, capability := range contract.Capabilities {
		if seen[capability.ID] {
			t.Fatalf("duplicate capability %q", capability.ID)
		}
		seen[capability.ID] = true

		entry, ok := capability.Layers["go"]
		if !ok {
			t.Fatalf("capability %q has no Go entry", capability.ID)
		}
		if entry.Status != "supported" && entry.Status != "partial" && entry.Status != "unavailable" {
			t.Fatalf("capability %q has invalid status %q", capability.ID, entry.Status)
		}
		if entry.Status != "supported" && entry.Notes == "" {
			t.Fatalf("capability %q requires notes", capability.ID)
		}

		for _, symbol := range entry.Symbols {
			var exists bool
			if strings.HasPrefix(symbol, "Client.") {
				_, exists = clientType.MethodByName(strings.TrimPrefix(symbol, "Client."))
			} else {
				_, exists = packageSymbols[symbol]
			}
			if entry.Status == "unavailable" && exists {
				t.Fatalf("capability %q: %s unexpectedly exists", capability.ID, symbol)
			}
			if entry.Status != "unavailable" && !exists {
				t.Fatalf("capability %q: %s is missing", capability.ID, symbol)
			}
		}
	}
}

func TestColumnUseJSONCompatibility(t *testing.T) {
	var legacy QueryAnalysis
	if err := json.Unmarshal([]byte(`{"shape":"select","projections":[]}`), &legacy); err != nil {
		t.Fatal(err)
	}
	if len(legacy.ColumnUses) != 0 {
		t.Fatal("legacy payload acquired column uses")
	}
	var current QueryAnalysis
	if err := json.Unmarshal([]byte(`{"columnUses":[{"context":"filter","scopePath":"root","expressionPath":"where_clause.this","expressionSql":"id > 0","references":[{"column":"id","sourceKind":"unknown","confidence":"unknown","unqualified":true,"span":{"start":7,"end":9}}]}]}`), &current); err != nil {
		t.Fatal(err)
	}
	ref := current.ColumnUses[0].References[0]
	if ref.Column != "id" || ref.Confidence != "unknown" || ref.Span.Start != 7 || ref.Span.End != 9 {
		t.Fatalf("invalid flattened reference: %#v", ref)
	}
}

func TestVersion(t *testing.T) {
	if Version() == "" {
		t.Fatal("Version() is empty")
	}
	if expected := os.Getenv("POLYGLOT_GO_EXPECTED_VERSION"); expected != "" && Version() != expected {
		t.Fatalf("Version() = %q, want %q", Version(), expected)
	}
}

func TestBuilderRequestUsesSharedProtocol(t *testing.T) {
	query := Select("customer_id", "COUNT(*) AS orders").
		From("orders").
		Where("status = 'complete'").
		GroupBy("customer_id")

	request, err := query.request(map[string]any{"kind": "sql", "dialect": "postgres"})
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if err := json.Unmarshal([]byte(request), &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded["version"] != float64(1) {
		t.Fatalf("version = %#v", decoded["version"])
	}
	if decoded["read_dialect"] != "generic" {
		t.Fatalf("read_dialect = %#v", decoded["read_dialect"])
	}
	if _, ok := decoded["expression"]; ok {
		t.Fatal("request unexpectedly contains the retired expression field")
	}
	plan, ok := decoded["plan"].(map[string]any)
	if !ok {
		t.Fatalf("plan missing or wrong type: %#v", decoded["plan"])
	}
	if _, ok := plan["base"].(map[string]any); !ok {
		t.Fatalf("plan base missing or wrong type: %#v", plan["base"])
	}
	operations, ok := plan["operations"].([]any)
	if !ok || len(operations) != 3 {
		t.Fatalf("plan operations = %#v, want three flat operations", plan["operations"])
	}
	if strings.Contains(request, `"kind":"apply"`) {
		t.Fatalf("request contains retired nested apply nodes: %s", request)
	}
}

func TestBuilderScalarStringsAreLiterals(t *testing.T) {
	request, err := Column("status").Eq("active").request(map[string]any{"kind": "ast"})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(request, `"kind":"string","value":"active"`) {
		t.Fatalf("request does not contain a string literal: %s", request)
	}
}

func TestBuilderFullFeatureSetUsesTypedPlanOperations(t *testing.T) {
	query := Select("department", Count(Star()).As("employees")).
		From("employees").
		FullJoin("departments", Column("employees.department_id").Eq(Column("departments.id"))).
		Window("w", []any{"department"}, []any{Column("salary").Desc()}).
		LateralViewWithOptions(LateralViewOptions{Outer: true}, Func("EXPLODE", Column("teams")), "team", "name").
		ForShare().
		Hint("FULL(employees)").
		CTASWithOptions(CTASOptions{Replace: true, Temporary: true}, "department_summary")

	request, err := query.request(map[string]any{"kind": "sql", "dialect": "generic"})
	if err != nil {
		t.Fatal(err)
	}
	for _, kind := range []string{"full", "window", "outer", "share", "hint", "ctas", "count", "temporary", "replace"} {
		if !strings.Contains(request, kind) {
			t.Fatalf("request does not contain %q: %s", kind, request)
		}
	}

	merge := MergeInto("target").
		MergeUsing("source", Column("target.id").Eq(Column("source.id"))).
		WhenMatchedUpdate(map[string]any{"name": Column("source.name")}, "source.active").
		WhenMatchedDelete("source.deleted").
		WhenNotMatchedInsert([]string{"id"}, []any{Column("source.id")}, "source.active")
	if _, err := merge.request(map[string]any{"kind": "ast"}); err != nil {
		t.Fatal(err)
	}
}

func TestColumnResolutionErrorSentinels(t *testing.T) {
	tests := []struct {
		status int32
		target error
	}{
		{status: 7, target: ErrColumnNotFound},
		{status: 8, target: ErrColumnIndeterminate},
		{status: 9, target: ErrColumnAmbiguous},
	}

	for _, test := range tests {
		err := &Error{Operation: "lineage_at", Status: test.status, Message: "resolution failed"}
		if !errors.Is(err, test.target) {
			t.Fatalf("errors.Is(%v, %v) = false", err, test.target)
		}
	}
}

func TestTranspileOptionsJSON(t *testing.T) {
	payload, err := marshalOptions(TranspileOptions{
		Pretty:           true,
		UnsupportedLevel: UnsupportedRaise,
		MaxUnsupported:   2,
	})
	if err != nil {
		t.Fatal(err)
	}
	if payload != `{"pretty":true,"unsupportedLevel":"raise","maxUnsupported":2}` {
		t.Fatalf("payload = %s", payload)
	}
}

func TestTranspileOptionsComplexityGuardJSON(t *testing.T) {
	limit := 128
	payload, err := marshalOptions(TranspileOptions{
		ComplexityGuard: &ComplexityGuardOptions{
			MaxFunctionCallDepth: &limit,
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	if payload != `{"complexityGuard":{"maxFunctionCallDepth":128}}` {
		t.Fatalf("payload = %s", payload)
	}
}

func TestParserDepthGuardJSON(t *testing.T) {
	for _, tc := range []struct {
		limit GuardLimit
		want  string
	}{
		{GuardLimit{}, `{}`},
		{NewGuardLimit(0), `{"maxParserDepth":0}`},
		{NewGuardLimit(64), `{"maxParserDepth":64}`},
		{DisabledGuardLimit(), `{"maxParserDepth":null}`},
	} {
		data, err := json.Marshal(ComplexityGuardOptions{MaxParserDepth: tc.limit})
		if err != nil || string(data) != tc.want {
			t.Fatalf("JSON = %s, %v; want %s", data, err, tc.want)
		}
		var decoded ComplexityGuardOptions
		if err := json.Unmarshal(data, &decoded); err != nil {
			t.Fatal(err)
		}
		data, err = json.Marshal(decoded)
		if err != nil || string(data) != tc.want {
			t.Fatalf("round trip = %s, %v", data, err)
		}
	}
	for _, data := range []string{`{"maxParserDepth":-1}`, `{"maxParserDepth":1.5}`, `{"maxParserDepth":true}`} {
		var decoded ComplexityGuardOptions
		if err := json.Unmarshal([]byte(data), &decoded); err == nil {
			t.Fatalf("accepted %s", data)
		}
	}
}

func TestFormatOptionsJSON(t *testing.T) {
	limit := 128
	payload, err := marshalOptions(FormatOptions{MaxSetOpChain: &limit})
	if err != nil {
		t.Fatal(err)
	}
	if payload != `{"maxSetOpChain":128}` {
		t.Fatalf("payload = %s", payload)
	}
}

func TestValidationOptionsJSON(t *testing.T) {
	payload, err := marshalOptions(ValidationOptions{StrictSyntax: true, Semantic: true})
	if err != nil {
		t.Fatal(err)
	}
	if payload != `{"strictSyntax":true,"semantic":true}` {
		t.Fatalf("payload = %s", payload)
	}
}

func TestSchemaValidationOptionsAndSchemaJSON(t *testing.T) {
	strict := false
	payload, err := marshalOptions(SchemaValidationOptions{
		CheckTypes: true, CheckReferences: true, Strict: &strict, StrictSyntax: true, Semantic: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	var options map[string]any
	if err := json.Unmarshal([]byte(payload), &options); err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"check_types", "check_references", "strict_syntax", "semantic"} {
		if options[key] != true {
			t.Fatalf("%s missing from %s", key, payload)
		}
	}
	if options["strict"] != false {
		t.Fatalf("strict=false lost: %s", payload)
	}
	payload, err = marshalOptions(SchemaValidationOptions{})
	if err != nil || payload != "{}" {
		t.Fatalf("default options = %s, %v", payload, err)
	}
	payload, err = marshalOptions(ValidationSchema{})
	if err != nil || payload != `{"tables":[]}` {
		t.Fatalf("empty schema = %s, %v", payload, err)
	}
	schema := ValidationSchema{Tables: []SchemaTable{{Name: "orders"}}}
	payload, err = marshalOptions(schema)
	if err != nil || !strings.Contains(payload, `"columns":[]`) {
		t.Fatalf("open schema = %s, %v", payload, err)
	}
	if schema.Tables[0].Columns != nil {
		t.Fatal("serialization mutated the caller's schema")
	}
}

func TestValidateWithSchemaInputAndLifecycleErrors(t *testing.T) {
	var client *Client
	if _, err := client.ValidateWithSchema("SELECT 1", ValidationSchema{}, ""); !errors.Is(err, ErrClosed) {
		t.Fatalf("nil client error = %v", err)
	}
	for _, input := range [][2]string{{"SELECT \x00", "generic"}, {"SELECT 1", "generic\x00"}} {
		if _, err := client.ValidateWithSchema(input[0], ValidationSchema{}, input[1]); err == nil || errors.Is(err, ErrClosed) {
			t.Fatalf("expected NUL validation error, got %v", err)
		}
	}
	if _, err := client.ValidateWithSchema("SELECT 1", ValidationSchema{}, "", SchemaValidationOptions{}, SchemaValidationOptions{}); err == nil || errors.Is(err, ErrClosed) {
		t.Fatalf("expected options count error, got %v", err)
	}
}

func TestOpenLineageOptionsDefaults(t *testing.T) {
	payload, err := marshalOpenLineageOptions(OpenLineageOptions{
		Producer: "test",
		OutputDataset: &OpenLineageDatasetID{
			Namespace: "warehouse",
			Name:      "out",
		},
	})
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]any
	if err := json.Unmarshal([]byte(payload), &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded["dialect"] != "generic" {
		t.Fatalf("dialect = %#v", decoded["dialect"])
	}
	if _, ok := decoded["datasetMappings"].(map[string]any); !ok {
		t.Fatalf("datasetMappings missing or wrong type: %#v", decoded["datasetMappings"])
	}
}

func TestAnalyzeQueryOptionsDefaults(t *testing.T) {
	payload, err := marshalAnalyzeQueryOptions(AnalyzeQueryOptions{})
	if err != nil {
		t.Fatal(err)
	}

	var decoded map[string]any
	if err := json.Unmarshal([]byte(payload), &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded["dialect"] != "generic" {
		t.Fatalf("dialect = %#v", decoded["dialect"])
	}
}

func TestDefaultClientMissingReturnsError(t *testing.T) {
	ClearDefaultClient()
	if _, err := DefaultClient(); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("DefaultClient err = %v, want ErrNoDefaultClient", err)
	}
	_, err := Transpile("SELECT 1", "generic", "generic")
	if !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("err = %v, want ErrNoDefaultClient", err)
	}
	if _, err := ParseDataType("INT", "generic"); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("ParseDataType err = %v, want ErrNoDefaultClient", err)
	}
	if _, err := GenerateDataType(json.RawMessage(`{"data_type":"int"}`), "generic"); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("GenerateDataType err = %v, want ErrNoDefaultClient", err)
	}
	if _, err := AnalyzeQuery("SELECT 1", AnalyzeQueryOptions{}); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("AnalyzeQuery err = %v, want ErrNoDefaultClient", err)
	}
	if _, err := SetLimit(json.RawMessage(`[]`), 1); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("SetLimit err = %v, want ErrNoDefaultClient", err)
	}
	if _, err := SetOffset(json.RawMessage(`[]`), 1); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("SetOffset err = %v, want ErrNoDefaultClient", err)
	}
	if _, err := SetOrderBy(json.RawMessage(`[]`), json.RawMessage(`[]`)); !errors.Is(err, ErrNoDefaultClient) {
		t.Fatalf("SetOrderBy err = %v, want ErrNoDefaultClient", err)
	}
}

func TestClosedClientReturnsError(t *testing.T) {
	client := &Client{closed: true}
	_, err := client.DialectCount()
	if !errors.Is(err, ErrClosed) {
		t.Fatalf("err = %v, want ErrClosed", err)
	}
}

func TestErrorWrapping(t *testing.T) {
	err := &Error{Operation: "transpile", Status: 3, Message: "bad sql"}
	if !strings.Contains(err.Error(), "transpile") || !strings.Contains(err.Error(), "bad sql") {
		t.Fatalf("unexpected error string: %s", err.Error())
	}
	if !errors.Is(err, &Error{Status: 3}) {
		t.Fatalf("errors.Is did not match status")
	}
	if !errors.Is(err, &Error{Operation: "transpile"}) {
		t.Fatalf("errors.Is did not match operation")
	}
}

func TestRejectNUL(t *testing.T) {
	if err := rejectNUL("abc"); err != nil {
		t.Fatalf("rejectNUL safe string: %v", err)
	}
	if err := rejectNUL("a\x00b"); err == nil {
		t.Fatalf("rejectNUL accepted embedded NUL")
	}
}

func TestLibraryFileName(t *testing.T) {
	name := libraryFileName()
	switch runtime.GOOS {
	case "darwin":
		if name != "libpolyglot_sql_ffi.dylib" {
			t.Fatalf("name = %q", name)
		}
	case "windows":
		if name != "polyglot_sql_ffi.dll" {
			t.Fatalf("name = %q", name)
		}
	default:
		if name != "libpolyglot_sql_ffi.so" {
			t.Fatalf("name = %q", name)
		}
	}
}

func TestDefaultLibraryCandidatesIncludeEnvFirst(t *testing.T) {
	t.Setenv(LibraryPathEnv, "/tmp/custom-polyglot-lib")
	candidates := defaultLibraryCandidates()
	if len(candidates) == 0 {
		t.Fatal("no candidates")
	}
	if candidates[0] != "/tmp/custom-polyglot-lib" {
		t.Fatalf("first candidate = %q", candidates[0])
	}
	if candidates[len(candidates)-1] != libraryFileName() {
		t.Fatalf("last candidate = %q, want system name", candidates[len(candidates)-1])
	}
}
