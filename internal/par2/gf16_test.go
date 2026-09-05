package par2

import (
	"testing"

	"github.com/javi11/par2go"
)

func TestGF16MethodFromConfig(t *testing.T) {
	tests := []struct {
		name    string
		in      string
		want    int
		wantErr bool
	}{
		{"empty is auto", "", par2go.GF16Auto, false},
		{"auto", "auto", par2go.GF16Auto, false},
		{"lookup", "lookup", par2go.GF16Lookup, false},
		{"lookup3", "lookup3", par2go.GF16Lookup3, false},
		{"shuffle-avx2", "shuffle-avx2", par2go.GF16ShuffleAVX2, false},
		{"shuffle-avx512", "shuffle-avx512", par2go.GF16ShuffleAVX512, false},
		{"shuffle-vbmi", "shuffle-vbmi", par2go.GF16ShuffleVBMI, false},
		{"xor-jit-avx2", "xor-jit-avx2", par2go.GF16XorJitAVX2, false},
		{"affine-avx2", "affine-avx2", par2go.GF16AffineAVX2, false},
		{"affine-avx512", "affine-avx512", par2go.GF16AffineAVX512, false},
		{"shuffle-neon", "shuffle-neon", par2go.GF16ShuffleNEON, false},
		{"clmul-neon", "clmul-neon", par2go.GF16ClmulNEON, false},
		{"unknown", "bogus", 0, true},
		{"wrong case", "Lookup", 0, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := gf16MethodFromConfig(tt.in)
			if tt.wantErr {
				if err == nil {
					t.Fatalf("gf16MethodFromConfig(%q) = %d, want error", tt.in, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("gf16MethodFromConfig(%q): unexpected error: %v", tt.in, err)
			}
			if got != tt.want {
				t.Errorf("gf16MethodFromConfig(%q) = %d, want %d", tt.in, got, tt.want)
			}
		})
	}
}
