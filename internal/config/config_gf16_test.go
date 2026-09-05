package config

import (
	"strings"
	"testing"
)

func TestGetDefaultConfig_Par2GF16MethodIsAuto(t *testing.T) {
	cfg := GetDefaultConfig()
	if cfg.Par2.GF16Method != Par2GF16MethodAuto {
		t.Errorf("Par2.GF16Method default = %q, want %q", cfg.Par2.GF16Method, Par2GF16MethodAuto)
	}
}

func TestValidate_Par2GF16Method(t *testing.T) {
	for _, method := range Par2GF16Methods {
		t.Run(method, func(t *testing.T) {
			cfg := validBaseConfig()
			cfg.Par2.GF16Method = method
			if err := cfg.Validate(); err != nil {
				t.Errorf("Validate() with gf16_method=%q: unexpected error: %v", method, err)
			}
		})
	}

	t.Run("empty is accepted as auto", func(t *testing.T) {
		cfg := validBaseConfig()
		cfg.Par2.GF16Method = ""
		if err := cfg.Validate(); err != nil {
			t.Errorf("Validate() with empty gf16_method: unexpected error: %v", err)
		}
	})

	t.Run("bogus is rejected", func(t *testing.T) {
		cfg := validBaseConfig()
		cfg.Par2.GF16Method = "bogus"
		err := cfg.Validate()
		if err == nil {
			t.Fatal("Validate() = nil, want error for gf16_method=bogus")
		}
		msg := err.Error()
		if !strings.Contains(msg, "gf16_method") {
			t.Errorf("error %q should name the field gf16_method", msg)
		}
		for _, method := range Par2GF16Methods {
			if !strings.Contains(msg, method) {
				t.Errorf("error %q should list accepted value %q", msg, method)
			}
		}
	})
}
