package ffi

import "unsafe"

const (
	StatusSuccess             int32 = 0
	StatusParseError          int32 = 1
	StatusGenerateError       int32 = 2
	StatusTranspileError      int32 = 3
	StatusValidationError     int32 = 4
	StatusInvalidArgument     int32 = 5
	StatusSerializationError  int32 = 6
	StatusColumnNotFound      int32 = 7
	StatusColumnIndeterminate int32 = 8
	StatusColumnAmbiguous     int32 = 9
	StatusInternalError       int32 = 99
)

// Result mirrors polyglot_result_t from crates/polyglot-sql-ffi.
type Result struct {
	Data   *byte
	Error  *byte
	Status int32
}

// ValidationResult mirrors polyglot_validation_result_t from crates/polyglot-sql-ffi.
type ValidationResult struct {
	Valid      int32
	ErrorsJSON *byte
	Error      *byte
	Status     int32
}

// CString copies a NUL-terminated native string. The caller must keep the
// allocation alive until this function returns. Native pointers remain typed
// pointers throughout the binding; no uintptr-to-pointer conversion is needed.
func CString(ptr *byte) string {
	if ptr == nil {
		return ""
	}

	p := unsafe.Pointer(ptr)
	n := 0
	for *(*byte)(unsafe.Add(p, n)) != 0 {
		n++
	}
	return string(unsafe.Slice((*byte)(p), n))
}
