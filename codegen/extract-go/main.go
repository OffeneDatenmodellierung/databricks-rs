// Command extract-go reads a databricks-sdk-go checkout and writes the
// intermediate representation (IR) that the Rust generator consumes.
//
// The Go SDK is itself generated from Databricks' OpenAPI spec, so its
// models (struct tags), impl.go (paths, verbs, query/body placement,
// pagination) and api.go (docs, long-running-operation waiters) carry the
// same information as the spec. This is the "Go front-end"; an OpenAPI
// front-end can later produce the same IR.
//
//	go run . -sdk /path/to/databricks-sdk-go -out ../../spec/ir.json
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

// ---------------------------------------------------------------- IR types

type IR struct {
	Source   Source              `json:"source"`
	Services []*Service          `json:"services"`
	Skipped  []string            `json:"skipped,omitempty"`
	Packages map[string]*Package `json:"packages"`
}

type Source struct {
	GoSDKVersion string `json:"go_sdk_version"`
	OpenAPISHA   string `json:"openapi_sha"`
}

type Package struct {
	Name  string     `json:"name"`
	Types []*TypeDef `json:"types"`
}

type TypeDef struct {
	Name   string       `json:"name"`
	Doc    string       `json:"doc,omitempty"`
	Kind   string       `json:"kind"` // struct | enum | alias
	Fields []*Field     `json:"fields,omitempty"`
	Values []*EnumValue `json:"values,omitempty"`
	Alias  *TypeRef     `json:"alias,omitempty"`
}

type Field struct {
	Name     string   `json:"name"` // wire name (json, url or header tag)
	GoName   string   `json:"go_name"`
	Doc      string   `json:"doc,omitempty"`
	Type     *TypeRef `json:"type"`
	Required bool     `json:"required"`
	// body | query | path | header — primary location. A field can carry
	// both a json and a url tag: it is then in the body for POST-like
	// verbs and in the query string for GET-like verbs.
	Location string `json:"location"`
	JSON     string `json:"json,omitempty"`  // json tag name ("" = never in a body)
	Query    string `json:"query,omitempty"` // url tag name ("" = never in a query)
}

type EnumValue struct {
	GoName string `json:"go_name"`
	Value  string `json:"value"`
	Doc    string `json:"doc,omitempty"`
}

type TypeRef struct {
	// string bool int int64 float64 any timestamp duration field_mask binary ref list map
	Kind string   `json:"kind"`
	Pkg  string   `json:"pkg,omitempty"`
	Name string   `json:"name,omitempty"`
	Elem *TypeRef `json:"elem,omitempty"`
}

type Service struct {
	Client string `json:"client"` // workspace | account
	// Parent service for nested accessors (w.settings().default_namespace()).
	Parent   string    `json:"parent,omitempty"`
	Accessor string    `json:"accessor"`
	Package  string    `json:"package"`
	Name     string    `json:"name"`
	Doc      string    `json:"doc,omitempty"`
	Methods  []*Method `json:"methods"`
	Waiters  []*Waiter `json:"waiters,omitempty"`
	// Go's generated name lookups (`XNameToIdMap`, list-based `GetByX`).
	Lookups []*Lookup `json:"lookups,omitempty"`
}

// Lookup is a generated Go helper that lists everything with `List` and
// either maps `Key` to `Value` (kind "map", duplicates are an error) or
// returns the single item whose `Key` equals a name (kind "get"). Key and
// Value are wire paths on the listed item type.
type Lookup struct {
	Name  string   `json:"name"`
	Kind  string   `json:"kind"`
	List  string   `json:"list"`
	Key   []string `json:"key"`
	Value []string `json:"value,omitempty"`
}

type PathPart struct {
	Lit       string `json:"lit,omitempty"`
	Field     string `json:"field,omitempty"` // wire name of the request field
	AccountID bool   `json:"account_id,omitempty"`
	// Escape each "/"-separated segment rather than leaving the value raw.
	MultiSegment bool `json:"multi_segment,omitempty"`
}

type QueryParam struct {
	Name      string `json:"name"`
	Field     string `json:"field"` // wire name of the request field
	FieldMask bool   `json:"field_mask,omitempty"`
}

type Method struct {
	Name            string        `json:"name"`
	Doc             string        `json:"doc,omitempty"`
	Verb            string        `json:"verb"`
	Path            []*PathPart   `json:"path"`
	Request         *TypeRef      `json:"request,omitempty"`
	Response        *TypeRef      `json:"response,omitempty"`
	BodyField       string        `json:"body_field,omitempty"`
	ExplicitQuery   []*QueryParam `json:"explicit_query,omitempty"`
	Accept          string        `json:"accept,omitempty"`
	ContentType     string        `json:"content_type,omitempty"`
	WorkspaceHeader bool          `json:"workspace_header"`
	Pagination      *Pagination   `json:"pagination,omitempty"`
	Wait            *WaitBinding  `json:"wait,omitempty"`
	// Long-running operation handle the Go method returns.
	Lro *Lro `json:"lro,omitempty"`
	// Fields the Go method sets on the request before sending it.
	RequestInit []*FieldInit `json:"request_init,omitempty"`
	Unsupported string       `json:"unsupported,omitempty"`
}

// FieldInit is one request field the Go SDK fills in before a call:
//   - request.StartIndex = 1                       -> always, value 1
//   - if request.Count == 0 { request.Count = N }  -> unset, value N
//   - ForceSendFields append "MaxResults"          -> unset, value 0 (sent even when zero)
//   - if request.RequestId == "" { ...uuid... }    -> unset, uuid
type FieldInit struct {
	Field string `json:"field"`           // wire name
	When  string `json:"when"`            // always | unset
	Value string `json:"value,omitempty"` // Go literal
	UUID  bool   `json:"uuid,omitempty"`
}

type Pagination struct {
	// token | offset | page | single
	Kind        string   `json:"kind"`
	Items       string   `json:"items"` // wire name on the response
	ItemType    *TypeRef `json:"item_type"`
	RespField   string   `json:"resp_field,omitempty"`
	ReqField    string   `json:"req_field,omitempty"`
	StopOnEmpty bool     `json:"stop_on_empty,omitempty"`
	DedupeKey   string   `json:"dedupe_key,omitempty"`
}

// Lro describes Go's typed long-running-operation wrapper
// (`XOperationInterface`): the operation is polled with `Poll` (a method of
// the same service, taking `{name}`) until done; the result is `Result`
// decoded from `response` (none for deletes), and `Metadata` from
// `metadata`. `Cancel` is the service method that cancels it, if any.
type Lro struct {
	Result   *TypeRef `json:"result,omitempty"`
	Metadata *TypeRef `json:"metadata,omitempty"`
	Poll     string   `json:"poll"`
	Cancel   string   `json:"cancel,omitempty"`
}

type WaitBinding struct {
	Waiter         string `json:"waiter"`
	FromResponse   bool   `json:"from_response"`
	Field          string `json:"field"` // wire name
	TimeoutMinutes int    `json:"timeout_minutes"`
}

type Waiter struct {
	Name        string   `json:"name"`
	PollMethod  string   `json:"poll_method"`
	Param       string   `json:"param"` // wire name on the poll request
	ParamType   *TypeRef `json:"param_type"`
	Result      *TypeRef `json:"result"`
	StatusPath  []string `json:"status_path"`
	MessagePath []string `json:"message_path,omitempty"`
	Targets     []string `json:"targets"`
	Failures    []string `json:"failures,omitempty"`
}

// ---------------------------------------------------------------- parsing

type pkgInfo struct {
	name      string
	files     map[string]*ast.File
	types     map[string]*TypeDef
	typeOrder []string
	consts    map[string]*EnumValue // const name -> value
	constType map[string]string     // const name -> enum type
	// intermediate: go field name -> wire name per struct
	goToWire map[string]map[string]string
	goType   map[string]map[string]*TypeRef
}

var fset = token.NewFileSet()

func must(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func parseDir(dir string) map[string]*ast.File {
	out := map[string]*ast.File{}
	entries, err := os.ReadDir(dir)
	must(err)
	for _, e := range entries {
		n := e.Name()
		if !strings.HasSuffix(n, ".go") || strings.HasSuffix(n, "_test.go") {
			continue
		}
		f, err := parser.ParseFile(fset, filepath.Join(dir, n), nil, parser.ParseComments)
		must(err)
		out[n] = f
	}
	return out
}

func docText(cg *ast.CommentGroup) string {
	if cg == nil {
		return ""
	}
	return strings.TrimSpace(cg.Text())
}

var tagRe = regexp.MustCompile(`(\w+):"([^"]*)"`)

func parseTag(lit *ast.BasicLit) map[string]string {
	out := map[string]string{}
	if lit == nil {
		return out
	}
	raw, _ := strconv.Unquote(lit.Value)
	for _, m := range tagRe.FindAllStringSubmatch(raw, -1) {
		out[m[1]] = m[2]
	}
	return out
}

func typeRef(pkg string, e ast.Expr) *TypeRef {
	switch t := e.(type) {
	case *ast.StarExpr:
		return typeRef(pkg, t.X)
	case *ast.ArrayType:
		return &TypeRef{Kind: "list", Elem: typeRef(pkg, t.Elt)}
	case *ast.MapType:
		return &TypeRef{Kind: "map", Elem: typeRef(pkg, t.Value)}
	case *ast.InterfaceType:
		return &TypeRef{Kind: "any"}
	case *ast.Ident:
		switch t.Name {
		case "string":
			return &TypeRef{Kind: "string"}
		case "bool":
			return &TypeRef{Kind: "bool"}
		case "int", "int32":
			return &TypeRef{Kind: "int"}
		case "int64":
			return &TypeRef{Kind: "int64"}
		case "float64", "float32":
			return &TypeRef{Kind: "float64"}
		case "any":
			return &TypeRef{Kind: "any"}
		}
		return &TypeRef{Kind: "ref", Pkg: pkg, Name: t.Name}
	case *ast.SelectorExpr:
		x := t.X.(*ast.Ident).Name
		switch x + "." + t.Sel.Name {
		case "time.Time":
			return &TypeRef{Kind: "timestamp"}
		case "duration.Duration":
			return &TypeRef{Kind: "duration"}
		case "fieldmask.FieldMask":
			return &TypeRef{Kind: "field_mask"}
		case "json.RawMessage":
			return &TypeRef{Kind: "any"}
		case "io.ReadCloser", "io.Reader":
			return &TypeRef{Kind: "binary"}
		}
		return &TypeRef{Kind: "ref", Pkg: x, Name: t.Sel.Name}
	}
	return &TypeRef{Kind: "any"}
}

func loadModels(p *pkgInfo) {
	f, ok := p.files["model.go"]
	if !ok {
		return
	}
	for _, d := range f.Decls {
		gd, ok := d.(*ast.GenDecl)
		if !ok {
			continue
		}
		switch gd.Tok {
		case token.TYPE:
			for _, s := range gd.Specs {
				ts := s.(*ast.TypeSpec)
				doc := docText(gd.Doc)
				if ts.Doc != nil {
					doc = docText(ts.Doc)
				}
				td := &TypeDef{Name: ts.Name.Name, Doc: doc}
				switch t := ts.Type.(type) {
				case *ast.StructType:
					td.Kind = "struct"
					p.goToWire[td.Name] = map[string]string{}
					p.goType[td.Name] = map[string]*TypeRef{}
					for _, fl := range t.Fields.List {
						if len(fl.Names) == 0 {
							continue
						}
						goName := fl.Names[0].Name
						if goName == "ForceSendFields" {
							continue
						}
						tags := parseTag(fl.Tag)
						f := &Field{GoName: goName, Doc: docText(fl.Doc), Type: typeRef(p.name, fl.Type)}
						jsonName, jsonOpts := splitTag(tags["json"])
						urlName, urlOpts := splitTag(tags["url"])
						hdrName, _ := splitTag(tags["header"])
						if jsonName != "-" {
							f.JSON = jsonName
						}
						if urlName != "-" {
							f.Query = urlName
						}
						switch {
						case jsonName != "" && jsonName != "-":
							f.Name, f.Location = jsonName, "body"
							f.Required = !strings.Contains(jsonOpts, "omitempty")
						case urlName != "" && urlName != "-":
							f.Name, f.Location = urlName, "query"
							f.Required = !strings.Contains(urlOpts, "omitempty")
						case hdrName != "" && hdrName != "-":
							f.Name, f.Location = hdrName, "header"
						default:
							f.Name, f.Location = snake(goName), "path"
							f.Required = true
						}
						td.Fields = append(td.Fields, f)
						p.goToWire[td.Name][goName] = f.Name
						p.goType[td.Name][goName] = f.Type
					}
				case *ast.Ident:
					if t.Name == "string" {
						td.Kind = "enum"
					} else {
						td.Kind = "alias"
						td.Alias = typeRef(p.name, t)
					}
				default:
					td.Kind = "alias"
					td.Alias = typeRef(p.name, ts.Type)
				}
				p.types[td.Name] = td
				p.typeOrder = append(p.typeOrder, td.Name)
			}
		case token.CONST:
			for _, s := range gd.Specs {
				vs := s.(*ast.ValueSpec)
				if vs.Type == nil || len(vs.Values) != 1 {
					continue
				}
				id, ok := vs.Type.(*ast.Ident)
				if !ok {
					continue
				}
				lit, ok := vs.Values[0].(*ast.BasicLit)
				if !ok || lit.Kind != token.STRING {
					continue
				}
				v, _ := strconv.Unquote(lit.Value)
				doc := docText(vs.Doc)
				if doc == "" {
					doc = docText(gd.Doc)
				}
				ev := &EnumValue{GoName: vs.Names[0].Name, Value: v, Doc: doc}
				p.consts[ev.GoName] = ev
				p.constType[ev.GoName] = id.Name
			}
		}
	}
	for name, typ := range p.constType {
		if td, ok := p.types[typ]; ok && td.Kind == "enum" {
			td.Values = append(td.Values, p.consts[name])
		}
	}
	for _, td := range p.types {
		sort.Slice(td.Values, func(i, j int) bool { return td.Values[i].Value < td.Values[j].Value })
	}
}

func splitTag(t string) (string, string) {
	if t == "" {
		return "", ""
	}
	parts := strings.SplitN(t, ",", 2)
	if len(parts) == 1 {
		return parts[0], ""
	}
	return parts[0], parts[1]
}

var snakeRe1 = regexp.MustCompile(`([a-z0-9])([A-Z])`)
var snakeRe2 = regexp.MustCompile(`([A-Z]+)([A-Z][a-z])`)

func snake(s string) string {
	s = snakeRe2.ReplaceAllString(s, "${1}_${2}")
	s = snakeRe1.ReplaceAllString(s, "${1}_${2}")
	return strings.ToLower(s)
}

// ---------------------------------------------------------------- impl.go

type implFunc struct {
	decl *ast.FuncDecl
}

func exprString(e ast.Expr) string {
	switch t := e.(type) {
	case *ast.Ident:
		return t.Name
	case *ast.SelectorExpr:
		return exprString(t.X) + "." + t.Sel.Name
	case *ast.CallExpr:
		return exprString(t.Fun) + "()"
	case *ast.StarExpr:
		return "*" + exprString(t.X)
	case *ast.UnaryExpr:
		return t.Op.String() + exprString(t.X)
	case *ast.IndexExpr:
		return exprString(t.X) + "[" + exprString(t.Index) + "]"
	case *ast.BasicLit:
		return t.Value
	case *ast.CompositeLit:
		return exprString(t.Type) + "{}"
	case *ast.ArrayType:
		return "[]" + exprString(t.Elt)
	}
	return fmt.Sprintf("%T", e)
}

func strLit(e ast.Expr) (string, bool) {
	lit, ok := e.(*ast.BasicLit)
	if !ok || lit.Kind != token.STRING {
		return "", false
	}
	v, err := strconv.Unquote(lit.Value)
	return v, err == nil
}

// requestWire converts `request.Foo` to the wire name of Foo on reqType.
func (p *pkgInfo) requestWire(reqType, expr string) (string, bool) {
	if !strings.HasPrefix(expr, "request.") {
		return "", false
	}
	goName := strings.TrimPrefix(expr, "request.")
	if strings.Contains(goName, ".") {
		return "", false
	}
	w, ok := p.goToWire[reqType][goName]
	return w, ok
}

func (p *pkgInfo) parseOp(m *Method, fd *ast.FuncDecl, reqType string) {
	// Request type from the signature.
	for _, prm := range fd.Type.Params.List[1:] {
		m.Request = typeRef(p.name, prm.Type)
		reqType = m.Request.Name
	}
	// Response type from the first result, when not an error.
	res := fd.Type.Results.List
	if len(res) == 2 {
		m.Response = typeRef(p.name, res[0].Type)
	}
	var updateMaskField string
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		switch s := n.(type) {
		case *ast.AssignStmt:
			if len(s.Lhs) != 1 || len(s.Rhs) != 1 {
				// updateMaskJson, err := json.Marshal(request.UpdateMask)
				if len(s.Rhs) == 1 {
					if call, ok := s.Rhs[0].(*ast.CallExpr); ok && exprString(call.Fun) == "json.Marshal" {
						if w, ok := p.requestWire(reqType, exprString(call.Args[0])); ok {
							updateMaskField = w
						}
					}
				}
				return true
			}
			lhs := exprString(s.Lhs[0])
			switch {
			case lhs == "path":
				m.Path = p.parsePath(reqType, s.Rhs[0], m)
			case strings.HasPrefix(lhs, "queryParams["):
				ix := s.Lhs[0].(*ast.IndexExpr)
				name, _ := strLit(ix.Index)
				rhs := exprString(s.Rhs[0])
				if w, ok := p.requestWire(reqType, rhs); ok {
					m.ExplicitQuery = append(m.ExplicitQuery, &QueryParam{Name: name, Field: w})
				} else if strings.HasPrefix(rhs, "strings.Trim") && updateMaskField != "" {
					m.ExplicitQuery = append(m.ExplicitQuery, &QueryParam{Name: name, Field: updateMaskField, FieldMask: true})
				} else {
					m.Unsupported = "unrecognised query param " + name
				}
			case strings.HasPrefix(lhs, "headers["):
				ix := s.Lhs[0].(*ast.IndexExpr)
				name, _ := strLit(ix.Index)
				val, isLit := strLit(s.Rhs[0])
				switch {
				case name == "Accept" && isLit:
					m.Accept = val
				case name == "Content-Type" && isLit:
					m.ContentType = val
				case name == "X-Databricks-Workspace-Id":
					m.WorkspaceHeader = true
				default:
					if m.Unsupported == "" {
						m.Unsupported = "header " + name
					}
				}
			}
		case *ast.CallExpr:
			if exprString(s.Fun) != "a.client.Do" {
				return true
			}
			verb := exprString(s.Args[1])
			m.Verb = strings.ToUpper(strings.TrimPrefix(verb, "http.Method"))
			body := exprString(s.Args[5])
			if strings.HasPrefix(body, "request.") {
				if w, ok := p.requestWire(reqType, body); ok {
					m.BodyField = w
				} else {
					m.Unsupported = "body " + body
				}
			}
		}
		return true
	})
	if m.Accept != "" && m.Accept != "application/json" {
		m.Unsupported = "accept " + m.Accept
	}
	if m.ContentType != "" && m.ContentType != "application/json" {
		m.Unsupported = "content-type " + m.ContentType
	}
	if reqType != "" {
		for _, f := range p.types[reqType].fieldsOrEmpty() {
			if f.Type.Kind == "binary" || f.Location == "header" {
				m.Unsupported = "binary/header field " + f.Name
			}
		}
	}
	if m.Response != nil && m.Response.Kind == "ref" {
		if td := p.types[m.Response.Name]; td != nil {
			for _, f := range td.Fields {
				if f.Type.Kind == "binary" {
					m.Unsupported = "binary response"
				}
			}
		}
	}
}

func (t *TypeDef) fieldsOrEmpty() []*Field {
	if t == nil {
		return nil
	}
	return t.Fields
}

var verbRe = regexp.MustCompile(`%v`)

func (p *pkgInfo) parsePath(reqType string, e ast.Expr, m *Method) []*PathPart {
	if s, ok := strLit(e); ok {
		return []*PathPart{{Lit: s}}
	}
	call, ok := e.(*ast.CallExpr)
	if !ok || exprString(call.Fun) != "fmt.Sprintf" {
		m.Unsupported = "path expression"
		return nil
	}
	format, _ := strLit(call.Args[0])
	lits := verbRe.Split(format, -1)
	var out []*PathPart
	for i, lit := range lits {
		if lit != "" {
			out = append(out, &PathPart{Lit: lit})
		}
		if i+1 >= len(lits) {
			break
		}
		argExpr := call.Args[i+1]
		multi := false
		if c, ok := argExpr.(*ast.CallExpr); ok && exprString(c.Fun) == "httpclient.EncodeMultiSegmentPathParameter" {
			argExpr, multi = c.Args[0], true
		}
		arg := exprString(argExpr)
		switch {
		case arg == "a.client.ConfiguredAccountID()":
			out = append(out, &PathPart{AccountID: true})
		default:
			w, ok := p.requestWire(reqType, arg)
			if !ok {
				m.Unsupported = "path arg " + arg
				continue
			}
			goField := strings.TrimPrefix(arg, "request.")
			if !multi {
				multi = p.isResourceName(reqType, goField, m.Name, lastLit(out))
			}
			out = append(out, &PathPart{Field: w, MultiSegment: multi})
		}
	}
	return out
}

var resourcePattern = regexp.MustCompile(`[a-z][a-z_-]*/\{[a-z_]+\}`)
var versionSuffix = regexp.MustCompile(`/(v\d+|\d+\.\d+)/$`)

// isResourceName reports whether a string path parameter holds a
// hierarchical resource name (`projects/{p}/branches/{b}`) whose slashes are
// path separators. The released Go SDK inserts every value raw; open PR
// databricks-sdk-go#1811 escapes single-segment values (so `/` in, e.g., a
// column name no longer splits the path, issue #1765) and keeps `/` only in
// resource names. This heuristic reproduces that PR's classification for
// 766 of 769 parameters (the rest are values without `/`).
func (p *pkgInfo) isResourceName(reqType, goField, method, prevLit string) bool {
	t := p.goType[reqType][goField]
	if t == nil || t.Kind != "string" {
		return false
	}
	var doc string
	for _, f := range p.types[reqType].fieldsOrEmpty() {
		if f.GoName == goField {
			doc = f.Doc
		}
	}
	if resourcePattern.MatchString(doc) {
		return true
	}
	if goField != "Name" && goField != "Parent" {
		return false
	}
	return versionSuffix.MatchString(prevLit) || strings.Contains(method, "Operation")
}

func lastLit(parts []*PathPart) string {
	if len(parts) == 0 {
		return ""
	}
	return parts[len(parts)-1].Lit
}

// parseRequestInit reads the top-level statements of a public method that
// set request fields before the call (see FieldInit).
func (p *pkgInfo) parseRequestInit(fd *ast.FuncDecl, reqType string) []*FieldInit {
	var out []*FieldInit
	wire := func(e ast.Expr) (string, bool) { return p.requestWire(reqType, exprString(e)) }
	for _, st := range fd.Body.List {
		switch s := st.(type) {
		case *ast.AssignStmt:
			if len(s.Lhs) != 1 || len(s.Rhs) != 1 {
				continue
			}
			lhs := exprString(s.Lhs[0])
			if lhs == "request.ForceSendFields" {
				call, ok := s.Rhs[0].(*ast.CallExpr)
				if !ok || exprString(call.Fun) != "append" {
					continue
				}
				for _, a := range call.Args[1:] {
					if lit, ok := a.(*ast.BasicLit); ok {
						goName := strings.Trim(lit.Value, `"`)
						if w, ok := p.goToWire[reqType][goName]; ok {
							out = append(out, &FieldInit{Field: w, When: "unset", Value: "0"})
						}
					}
				}
				continue
			}
			if lit, ok := s.Rhs[0].(*ast.BasicLit); ok {
				if w, ok := wire(s.Lhs[0]); ok {
					out = append(out, &FieldInit{Field: w, When: "always", Value: lit.Value})
				}
			}
		case *ast.IfStmt:
			be, ok := s.Cond.(*ast.BinaryExpr)
			if !ok || be.Op != token.EQL || len(s.Body.List) != 1 {
				continue
			}
			w, ok := wire(be.X)
			if !ok {
				continue
			}
			as, ok := s.Body.List[0].(*ast.AssignStmt)
			if !ok || len(as.Rhs) != 1 || exprString(as.Lhs[0]) != exprString(be.X) {
				continue
			}
			switch r := as.Rhs[0].(type) {
			case *ast.BasicLit:
				out = append(out, &FieldInit{Field: w, When: "unset", Value: r.Value})
			case *ast.CallExpr:
				if strings.HasPrefix(exprString(r.Fun), "uuid.New()") {
					out = append(out, &FieldInit{Field: w, When: "unset", UUID: true})
				}
			}
		}
	}
	return out
}

// Pagination: public List(ctx, request) listing.Iterator[T] built from
// internalX + getItems + getNextReq.
func (p *pkgInfo) parsePagination(fd *ast.FuncDecl, respType string) (*Pagination, string) {
	pg := &Pagination{Kind: "single"}
	internal := ""
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		switch s := n.(type) {
		case *ast.CallExpr:
			fn := exprString(s.Fun)
			if strings.HasPrefix(fn, "a.internal") {
				internal = strings.TrimPrefix(fn, "a.")
			}
			if fn == "listing.NewDedupeIterator" && len(s.Args) >= 5 {
				// key func: func(item *T) string { return item.Id }
				if fl, ok := s.Args[4].(*ast.FuncLit); ok {
					ast.Inspect(fl.Body, func(n ast.Node) bool {
						if r, ok := n.(*ast.ReturnStmt); ok && len(r.Results) == 1 {
							pg.DedupeKey = exprString(r.Results[0])
						}
						return true
					})
				}
			}
		case *ast.AssignStmt:
			if len(s.Lhs) != 1 {
				return true
			}
			name := exprString(s.Lhs[0])
			fl, ok := s.Rhs[0].(*ast.FuncLit)
			if !ok {
				return true
			}
			switch name {
			case "getItems":
				ast.Inspect(fl.Body, func(n ast.Node) bool {
					if r, ok := n.(*ast.ReturnStmt); ok && len(r.Results) == 1 {
						pg.Items = strings.TrimPrefix(exprString(r.Results[0]), "resp.")
					}
					return true
				})
				if len(fl.Type.Results.List) == 1 {
					if at, ok := fl.Type.Results.List[0].Type.(*ast.ArrayType); ok {
						pg.ItemType = typeRef(p.name, at.Elt)
					}
				}
			case "getNextReq":
				ast.Inspect(fl.Body, func(n ast.Node) bool {
					switch st := n.(type) {
					case *ast.IfStmt:
						cond := exprString(st.Cond)
						if strings.HasPrefix(cond, "len(") || strings.Contains(cond, "getItems") {
							pg.StopOnEmpty = true
						}
						if be, ok := st.Cond.(*ast.BinaryExpr); ok && strings.Contains(exprString(be.X), "getItems") {
							pg.StopOnEmpty = true
						}
						if be, ok := st.Cond.(*ast.BinaryExpr); ok {
							if c, ok := be.X.(*ast.CallExpr); ok && exprString(c.Fun) == "len" {
								pg.StopOnEmpty = true
							}
						}
					case *ast.AssignStmt:
						lhs := exprString(st.Lhs[0])
						if !strings.HasPrefix(lhs, "request.") {
							return true
						}
						pg.ReqField = strings.TrimPrefix(lhs, "request.")
						switch r := st.Rhs[0].(type) {
						case *ast.SelectorExpr:
							pg.Kind = "token"
							pg.RespField = strings.TrimPrefix(exprString(r), "resp.")
						case *ast.BinaryExpr:
							pg.RespField = strings.TrimPrefix(exprString(r.X), "resp.")
							if lit, ok := r.Y.(*ast.BasicLit); ok && lit.Value == "1" {
								pg.Kind = "page"
							} else {
								pg.Kind = "offset"
							}
						}
					}
					return true
				})
			}
		}
		return true
	})
	return pg, internal
}

// parseLro reads the operation wrapper a public API method returns
// (`(CreateBranchOperationInterface, error)`).
func (p *pkgInfo) parseLro(fd *ast.FuncDecl, api *apiInfo) *Lro {
	res := fd.Type.Results.List
	if len(res) != 2 {
		return nil
	}
	iface := exprString(res[0].Type)
	if !strings.HasSuffix(iface, "OperationInterface") {
		return nil
	}
	op := strings.TrimSuffix(iface, "Interface")
	op = strings.ToLower(op[:1]) + op[1:]
	wait, ok := api.methods[op+".Wait"]
	if !ok {
		return nil
	}
	implCall := func(f *ast.FuncDecl) string {
		name := ""
		ast.Inspect(f.Body, func(n ast.Node) bool {
			if c, ok := n.(*ast.CallExpr); ok && name == "" {
				if fn := exprString(c.Fun); strings.HasPrefix(fn, "a.impl.") {
					name = strings.TrimPrefix(fn, "a.impl.")
				}
			}
			return true
		})
		return name
	}
	l := &Lro{Poll: implCall(wait)}
	if r := wait.Type.Results.List; len(r) == 2 {
		if st, ok := r[0].Type.(*ast.StarExpr); ok {
			l.Result = typeRef(p.name, st.X)
		}
	}
	if md, ok := api.methods[op+".Metadata"]; ok {
		if r := md.Type.Results.List; len(r) == 2 {
			if st, ok := r[0].Type.(*ast.StarExpr); ok {
				l.Metadata = typeRef(p.name, st.X)
			}
		}
	}
	if c, ok := api.methods[op+".Cancel"]; ok {
		l.Cancel = implCall(c)
	}
	if l.Poll == "" {
		return nil
	}
	return l
}

// parseLookups reads Go's generated name lookups on `<base>API`: bodies
// that call `a.<List>[All](ctx, …)` and build `mapping` (a name map) or
// `tmp` (list-based GetBy).
func (p *pkgInfo) parseLookups(base string, api *apiInfo, methods []*Method, warnings *[]string) []*Lookup {
	var out []*Lookup
	for _, key := range sortedKeys(api.methods) {
		if !strings.HasPrefix(key, base+"API.") {
			continue
		}
		fd := api.methods[key]
		var list string
		var keyPath, valPath []string
		kind := ""
		ast.Inspect(fd.Body, func(n ast.Node) bool {
			switch s := n.(type) {
			case *ast.CallExpr:
				if fn := exprString(s.Fun); strings.HasPrefix(fn, "a.") && list == "" {
					list = strings.TrimSuffix(strings.TrimPrefix(fn, "a."), "All")
				}
			case *ast.AssignStmt:
				if len(s.Lhs) != 1 || len(s.Rhs) != 1 {
					return true
				}
				lhs, rhs := exprString(s.Lhs[0]), exprString(s.Rhs[0])
				isMap := false
				if cl, ok := s.Rhs[0].(*ast.CompositeLit); ok {
					_, isMap = cl.Type.(*ast.MapType)
				}
				switch {
				case lhs == "mapping" && isMap:
					kind = "map"
				case lhs == "tmp" && isMap:
					kind = "get"
				case lhs == "key" && strings.HasPrefix(rhs, "v."):
					keyPath = strings.Split(strings.TrimPrefix(rhs, "v."), ".")
				case lhs == "mapping[key]" && strings.HasPrefix(rhs, "v."):
					valPath = strings.Split(strings.TrimPrefix(rhs, "v."), ".")
				}
			}
			return true
		})
		if kind == "" {
			continue
		}
		name := fd.Name.Name
		var item string
		for _, m := range methods {
			if m.Name != list {
				continue
			}
			switch {
			case m.Pagination != nil && m.Pagination.ItemType != nil:
				item = m.Pagination.ItemType.Name
			case m.Response != nil && m.Response.Kind == "list" && m.Response.Elem != nil:
				item = m.Response.Elem.Name
			}
		}
		wireKey, ok1 := p.goPathToWire(item, keyPath)
		wireVal, ok2 := p.goPathToWire(item, valPath)
		if item == "" || !ok1 || (kind == "map" && !ok2) {
			*warnings = append(*warnings, fmt.Sprintf("%s.%s: unresolved lookup", base, name))
			continue
		}
		l := &Lookup{Name: name, Kind: kind, List: list, Key: wireKey}
		if kind == "map" {
			l.Value = wireVal
		}
		out = append(out, l)
	}
	return out
}

// ---------------------------------------------------------------- api.go

type apiInfo struct {
	docs    map[string]map[string]string // iface -> method -> doc
	svcDoc  map[string]string            // API struct -> doc
	waiters map[string]*ast.FuncDecl     // "ClustersAPI.WaitGetClusterRunning"
	methods map[string]*ast.FuncDecl     // "ClustersAPI.Create"
	structs map[string]*ast.StructType   // "SettingsAPI"
}

func loadAPI(p *pkgInfo) *apiInfo {
	ai := &apiInfo{docs: map[string]map[string]string{}, svcDoc: map[string]string{}, waiters: map[string]*ast.FuncDecl{}, methods: map[string]*ast.FuncDecl{}, structs: map[string]*ast.StructType{}}
	f, ok := p.files["api.go"]
	if !ok {
		return ai
	}
	for _, d := range f.Decls {
		switch x := d.(type) {
		case *ast.GenDecl:
			for _, s := range x.Specs {
				ts, ok := s.(*ast.TypeSpec)
				if !ok {
					continue
				}
				doc := docText(x.Doc)
				switch t := ts.Type.(type) {
				case *ast.InterfaceType:
					m := map[string]string{}
					for _, fl := range t.Methods.List {
						if len(fl.Names) == 1 {
							m[fl.Names[0].Name] = docText(fl.Doc)
						}
					}
					ai.docs[ts.Name.Name] = m
				case *ast.StructType:
					if strings.HasSuffix(ts.Name.Name, "API") {
						ai.svcDoc[ts.Name.Name] = doc
						ai.structs[ts.Name.Name] = t
					}
				}
			}
		case *ast.FuncDecl:
			if x.Recv == nil || len(x.Recv.List) != 1 {
				continue
			}
			recv := strings.TrimPrefix(exprString(x.Recv.List[0].Type), "*")
			key := recv + "." + x.Name.Name
			if strings.HasPrefix(x.Name.Name, "Wait") && strings.HasSuffix(recv, "API") {
				ai.waiters[key] = x
			}
			ai.methods[key] = x
		}
	}
	return ai
}

// goPathToWire resolves e.g. ["State","LifeCycleState"] on type Run.
func (p *pkgInfo) goPathToWire(typ string, path []string) ([]string, bool) {
	var out []string
	for _, g := range path {
		w, ok := p.goToWire[typ][g]
		if !ok {
			return nil, false
		}
		out = append(out, w)
		t := p.goType[typ][g]
		if t != nil && t.Kind == "ref" {
			typ = t.Name
		}
	}
	return out, true
}

func (p *pkgInfo) parseWaiter(name string, fd *ast.FuncDecl) (*Waiter, error) {
	w := &Waiter{Name: strings.TrimPrefix(fd.Name.Name, "Wait")}
	// Param: second parameter (after ctx).
	prm := fd.Type.Params.List[1]
	paramGo := prm.Names[0].Name
	w.ParamType = typeRef(p.name, prm.Type)
	resultType := ""
	var respVar string
	var statusExpr, msgExpr []string
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		switch s := n.(type) {
		case *ast.AssignStmt:
			if len(s.Rhs) != 1 {
				return true
			}
			if call, ok := s.Rhs[0].(*ast.CallExpr); ok && len(s.Lhs) == 2 {
				fn := exprString(call.Fun)
				if strings.HasPrefix(fn, "a.") && len(call.Args) == 2 {
					w.PollMethod = strings.TrimPrefix(fn, "a.")
					respVar = exprString(s.Lhs[0])
					if cl, ok := call.Args[1].(*ast.CompositeLit); ok {
						for _, el := range cl.Elts {
							kv := el.(*ast.KeyValueExpr)
							if exprString(kv.Value) == paramGo {
								reqT := exprString(cl.Type)
								if wire, ok := p.goToWire[reqT][exprString(kv.Key)]; ok {
									w.Param = wire
								}
							}
						}
					}
				}
			}
			lhs := exprString(s.Lhs[0])
			rhs := exprString(s.Rhs[0])
			if lhs == "status" && strings.HasPrefix(rhs, respVar+".") {
				statusExpr = strings.Split(strings.TrimPrefix(rhs, respVar+"."), ".")
			}
			if lhs == "statusMessage" && strings.HasPrefix(rhs, respVar+".") {
				msgExpr = strings.Split(strings.TrimPrefix(rhs, respVar+"."), ".")
			}
		case *ast.CaseClause:
			var vals []string
			for _, e := range s.List {
				c := exprString(e)
				if ev, ok := p.consts[c]; ok {
					vals = append(vals, ev.Value)
				}
			}
			if len(vals) == 0 {
				return true
			}
			isTarget := false
			for _, st := range s.Body {
				if r, ok := st.(*ast.ReturnStmt); ok && len(r.Results) == 2 && exprString(r.Results[1]) == "nil" {
					isTarget = true
				}
			}
			if isTarget {
				w.Targets = append(w.Targets, vals...)
			} else {
				w.Failures = append(w.Failures, vals...)
			}
		case *ast.IndexListExpr, *ast.IndexExpr:
			// retries.Poll[ClusterDetails]
			var x ast.Expr
			var idx ast.Expr
			if il, ok := s.(*ast.IndexListExpr); ok {
				x, idx = il.X, il.Indices[0]
			} else {
				ie := s.(*ast.IndexExpr)
				x, idx = ie.X, ie.Index
			}
			if exprString(x) == "retries.Poll" {
				resultType = exprString(idx)
			}
		}
		return true
	})
	if resultType == "" || w.PollMethod == "" || len(statusExpr) == 0 || w.Param == "" {
		return nil, fmt.Errorf("waiter %s: incomplete (result=%q poll=%q status=%v param=%q)", name, resultType, w.PollMethod, statusExpr, w.Param)
	}
	w.Result = &TypeRef{Kind: "ref", Pkg: p.name, Name: resultType}
	sp, ok := p.goPathToWire(resultType, statusExpr)
	if !ok {
		return nil, fmt.Errorf("waiter %s: status path %v", name, statusExpr)
	}
	w.StatusPath = sp
	if len(msgExpr) > 0 {
		if mp, ok := p.goPathToWire(resultType, msgExpr); ok {
			w.MessagePath = mp
		}
	}
	// A nested message (e.g. run.State.StateMessage) is assigned inside an
	// `if x.State != nil` block; pick it up too.
	if len(w.MessagePath) == 0 {
		ast.Inspect(fd.Body, func(n ast.Node) bool {
			if s, ok := n.(*ast.AssignStmt); ok && len(s.Lhs) == 1 && exprString(s.Lhs[0]) == "statusMessage" {
				rhs := exprString(s.Rhs[0])
				if strings.HasPrefix(rhs, respVar+".") {
					if mp, ok := p.goPathToWire(resultType, strings.Split(strings.TrimPrefix(rhs, respVar+"."), ".")); ok {
						w.MessagePath = mp
					}
				}
			}
			return true
		})
	}
	return w, nil
}

// Trigger: public method returning *WaitX[R]; find the Poll binding.
func (p *pkgInfo) parseTrigger(fd *ast.FuncDecl, reqType, respType string) *WaitBinding {
	res := fd.Type.Results.List
	if len(res) != 2 {
		return nil
	}
	rt := exprString(res[0].Type)
	if !strings.HasPrefix(rt, "*Wait") {
		return nil
	}
	b := &WaitBinding{TimeoutMinutes: 20}
	b.Waiter = strings.TrimPrefix(strings.SplitN(rt, "[", 2)[0], "*Wait")
	reqVar := ""
	if ps := fd.Type.Params.List; len(ps) > 1 && len(ps[1].Names) == 1 {
		reqVar = ps[1].Names[0].Name
	}
	respVar := ""
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		if s, ok := n.(*ast.AssignStmt); ok && len(s.Lhs) == 2 {
			if c, ok := s.Rhs[0].(*ast.CallExpr); ok && strings.HasPrefix(exprString(c.Fun), "a.") {
				respVar = exprString(s.Lhs[0])
			}
		}
		return true
	})
	ast.Inspect(fd.Body, func(n ast.Node) bool {
		kv, ok := n.(*ast.KeyValueExpr)
		if !ok {
			return true
		}
		key := exprString(kv.Key)
		if key == "timeout" {
			if be, ok := kv.Value.(*ast.BinaryExpr); ok {
				if lit, ok := be.X.(*ast.BasicLit); ok {
					if v, err := strconv.Atoi(lit.Value); err == nil {
						b.TimeoutMinutes = v
					}
				}
			}
			return false
		}
		if key == "Response" || key == "Poll" || key == "callback" || b.Field != "" {
			return false
		}
		parts := strings.SplitN(exprString(kv.Value), ".", 2)
		if len(parts) != 2 {
			return true
		}
		switch parts[0] {
		case respVar:
			if w, ok := p.goToWire[respType][parts[1]]; ok {
				b.FromResponse, b.Field = true, w
			}
		case reqVar:
			if w, ok := p.goToWire[reqType][parts[1]]; ok {
				b.FromResponse, b.Field = false, w
			}
		}
		return false
	})
	if b.Field == "" {
		return nil
	}
	return b
}

// ---------------------------------------------------------------- clients

type clientField struct {
	client, accessor, pkg, iface, doc string
}

func loadClient(path, client string) []clientField {
	f, err := parser.ParseFile(fset, path, nil, parser.ParseComments)
	must(err)
	var out []clientField
	for _, d := range f.Decls {
		gd, ok := d.(*ast.GenDecl)
		if !ok {
			continue
		}
		for _, s := range gd.Specs {
			ts, ok := s.(*ast.TypeSpec)
			if !ok {
				continue
			}
			want := "WorkspaceClient"
			if client == "account" {
				want = "AccountClient"
			}
			if ts.Name.Name != want {
				continue
			}
			for _, fl := range ts.Type.(*ast.StructType).Fields.List {
				sel, ok := fl.Type.(*ast.SelectorExpr)
				if !ok || len(fl.Names) != 1 || !strings.HasSuffix(sel.Sel.Name, "Interface") {
					continue
				}
				out = append(out, clientField{client, fl.Names[0].Name, exprString(sel.X), sel.Sel.Name, docText(fl.Doc)})
			}
		}
	}
	return out
}

// ---------------------------------------------------------------- main

func main() {
	sdk := flag.String("sdk", "", "path to databricks-sdk-go checkout")
	out := flag.String("out", "ir.json", "output file")
	flag.Parse()
	if *sdk == "" {
		must(fmt.Errorf("-sdk is required"))
	}
	ver, _ := os.ReadFile(filepath.Join(*sdk, "version/version.go"))
	vm := regexp.MustCompile(`Version = "([^"]+)"`).FindSubmatch(ver)
	sha, _ := os.ReadFile(filepath.Join(*sdk, ".codegen/_openapi_sha"))
	ir := &IR{Packages: map[string]*Package{}}
	if vm != nil {
		ir.Source.GoSDKVersion = "v" + string(vm[1])
	}
	ir.Source.OpenAPISHA = strings.TrimSpace(string(sha))

	fields := append(loadClient(filepath.Join(*sdk, "workspace_client.go"), "workspace"),
		loadClient(filepath.Join(*sdk, "account_client.go"), "account")...)

	pkgs := map[string]*pkgInfo{}
	getPkg := func(name string) *pkgInfo {
		if p, ok := pkgs[name]; ok {
			return p
		}
		p := &pkgInfo{name: name, files: parseDir(filepath.Join(*sdk, "service", name)),
			types: map[string]*TypeDef{}, consts: map[string]*EnumValue{}, constType: map[string]string{},
			goToWire: map[string]map[string]string{}, goType: map[string]map[string]*TypeRef{}}
		loadModels(p)
		pkgs[name] = p
		return p
	}

	var warnings []string
	var build func(cf clientField, parent string)
	build = func(cf clientField, parent string) {
		p := getPkg(cf.pkg)
		api := loadAPI(p)
		base := strings.TrimSuffix(cf.iface, "Interface")
		implType := strings.ToLower(base[:1]) + base[1:] + "Impl"
		svc := &Service{Client: cf.client, Parent: parent, Accessor: cf.accessor, Package: cf.pkg, Name: base, Doc: cf.doc}
		if svc.Doc == "" {
			svc.Doc = api.svcDoc[base+"API"]
		}
		// Collect impl funcs.
		impl := map[string]*ast.FuncDecl{}
		var order []string
		for _, fname := range sortedKeys(p.files) {
			f := p.files[fname]
			for _, d := range f.Decls {
				fd, ok := d.(*ast.FuncDecl)
				if !ok || fd.Recv == nil {
					continue
				}
				if strings.TrimPrefix(exprString(fd.Recv.List[0].Type), "*") != implType {
					continue
				}
				impl[fd.Name.Name] = fd
				if fname == "impl.go" {
					order = append(order, fd.Name.Name)
				}
			}
		}
		docs := api.docs[cf.iface]
		for _, name := range order {
			if strings.HasPrefix(name, "internal") || strings.HasSuffix(name, "All") {
				continue
			}
			fd := impl[name]
			m := &Method{Name: name, Doc: docs[name]}
			resType := ""
			if r := fd.Type.Results.List; len(r) >= 1 {
				resType = exprString(r[0].Type)
			}
			if strings.HasPrefix(resType, "listing.Iterator") {
				reqT := ""
				if ps := fd.Type.Params.List; len(ps) > 1 {
					reqT = exprString(ps[1].Type)
				}
				pg, internal := p.parsePagination(fd, "")
				ifd, ok := impl[internal]
				if !ok {
					warnings = append(warnings, fmt.Sprintf("%s.%s: no internal op", base, name))
					continue
				}
				p.parseOp(m, ifd, reqT)
				pg.Items = p.wireOf(m.Response, pg.Items)
				if pg.RespField != "" {
					pg.RespField = p.wireOf(m.Response, pg.RespField)
				}
				if pg.ReqField != "" {
					pg.ReqField = p.wireOf(m.Request, pg.ReqField)
				}
				if pg.DedupeKey != "" {
					parts := strings.SplitN(pg.DedupeKey, ".", 2)
					if len(parts) == 2 && pg.ItemType != nil {
						pg.DedupeKey = p.goToWire[pg.ItemType.Name][parts[1]]
					}
				}
				m.Pagination = pg
			} else {
				p.parseOp(m, fd, "")
			}
			if m.Request != nil {
				m.RequestInit = p.parseRequestInit(fd, m.Request.Name)
			}
			if m.Verb == "" {
				warnings = append(warnings, fmt.Sprintf("%s.%s: no Do call (%s)", base, name, m.Unsupported))
				continue
			}
			// Waiter binding from the public API method.
			if afd, ok := api.methods[base+"API."+name]; ok {
				reqT, respT := "", ""
				if m.Request != nil {
					reqT = m.Request.Name
				}
				if m.Response != nil {
					respT = m.Response.Name
				}
				m.Wait = p.parseTrigger(afd, reqT, respT)
				m.Lro = p.parseLro(afd, api)
			}
			svc.Methods = append(svc.Methods, m)
		}
		svc.Lookups = p.parseLookups(base, api, svc.Methods, &warnings)
		for _, key := range sortedKeys(api.waiters) {
			if !strings.HasPrefix(key, base+"API.") {
				continue
			}
			w, err := p.parseWaiter(key, api.waiters[key])
			if err != nil {
				warnings = append(warnings, err.Error())
				continue
			}
			svc.Waiters = append(svc.Waiters, w)
		}
		// Nested sub-services: unexported XInterface fields on the API struct.
		var subs []clientField
		if st, ok := api.structs[base+"API"]; ok {
			for _, fl := range st.Fields.List {
				id, ok := fl.Type.(*ast.Ident)
				if !ok || len(fl.Names) != 1 || !strings.HasSuffix(id.Name, "Interface") {
					continue
				}
				n := fl.Names[0].Name
				subs = append(subs, clientField{cf.client, strings.ToUpper(n[:1]) + n[1:], cf.pkg, id.Name, docText(fl.Doc)})
			}
		}
		if len(svc.Methods) == 0 && len(subs) == 0 {
			ir.Skipped = append(ir.Skipped, fmt.Sprintf("%s %s.%s: no generated operations (hand-written or deprecated wrapper)", cf.client, cf.pkg, base))
			return
		}
		ir.Services = append(ir.Services, svc)
		for _, sub := range subs {
			build(sub, base)
		}
	}
	for _, cf := range fields {
		build(cf, "")
	}
	for _, name := range sortedKeys(pkgs) {
		p := pkgs[name]
		pk := &Package{Name: name}
		for _, t := range p.typeOrder {
			pk.Types = append(pk.Types, p.types[t])
		}
		ir.Packages[name] = pk
	}
	// Referenced packages that only provide types (no service of their own).
	for changed := true; changed; {
		changed = false
		for _, pk := range ir.Packages {
			for _, t := range pk.Types {
				for _, f := range t.Fields {
					for r := f.Type; r != nil; r = r.Elem {
						if r.Kind == "ref" {
							if _, ok := ir.Packages[r.Pkg]; !ok {
								p := getPkg(r.Pkg)
								np := &Package{Name: r.Pkg}
								for _, tn := range p.typeOrder {
									np.Types = append(np.Types, p.types[tn])
								}
								ir.Packages[r.Pkg] = np
								changed = true
							}
						}
					}
				}
			}
		}
	}
	data, err := json.MarshalIndent(ir, "", " ")
	must(err)
	must(os.WriteFile(*out, append(data, '\n'), 0o644))
	nm, nu := 0, 0
	for _, s := range ir.Services {
		for _, m := range s.Methods {
			nm++
			if m.Unsupported != "" {
				nu++
			}
		}
	}
	fmt.Fprintf(os.Stderr, "services=%d methods=%d unsupported=%d packages=%d warnings=%d\n", len(ir.Services), nm, nu, len(ir.Packages), len(warnings))
	for _, w := range warnings {
		fmt.Fprintln(os.Stderr, "  warn:", w)
	}
}

func (p *pkgInfo) wireOf(t *TypeRef, goName string) string {
	if t == nil || t.Kind != "ref" {
		return snake(goName)
	}
	if w, ok := p.goToWire[t.Name][goName]; ok {
		return w
	}
	return snake(goName)
}

func sortedKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
