package article

import (
	"io"
	"regexp"
	"strings"
	"testing"
	"time"
)

func TestNew(t *testing.T) {
	messageID := "test-message-id"
	subject := "Test Subject"
	originalSubject := "Original Subject"
	from := "test@example.com"
	groups := []string{"alt.test", "alt.binaries.test"}
	partNumber := 1
	totalParts := 3
	fileSize := int64(1000)
	fileName := "test.txt"
	fileNumber := 1
	originalName := "original.txt"
	customHeaders := map[string]string{"X-Test": "Value"}

	article := New(
		messageID,
		subject,
		originalSubject,
		from,
		groups,
		partNumber,
		totalParts,
		fileSize,
		fileName,
		fileNumber,
		originalName,
		customHeaders,
	)

	if article.MessageID != messageID {
		t.Errorf("Expected MessageID to be %s, got %s", messageID, article.MessageID)
	}
	if article.Subject != subject {
		t.Errorf("Expected Subject to be %s, got %s", subject, article.Subject)
	}
	if article.OriginalSubject != originalSubject {
		t.Errorf("Expected OriginalSubject to be %s, got %s", originalSubject, article.OriginalSubject)
	}
	if article.From != from {
		t.Errorf("Expected From to be %s, got %s", from, article.From)
	}
	if len(article.Groups) != len(groups) {
		t.Errorf("Expected Groups length to be %d, got %d", len(groups), len(article.Groups))
	}
	if article.PartNumber != partNumber {
		t.Errorf("Expected PartNumber to be %d, got %d", partNumber, article.PartNumber)
	}
	if article.TotalParts != totalParts {
		t.Errorf("Expected TotalParts to be %d, got %d", totalParts, article.TotalParts)
	}
	if article.FileSize != fileSize {
		t.Errorf("Expected FileSize to be %d, got %d", fileSize, article.FileSize)
	}
	if article.FileName != fileName {
		t.Errorf("Expected FileName to be %s, got %s", fileName, article.FileName)
	}
	if article.FileNumber != fileNumber {
		t.Errorf("Expected FileNumber to be %d, got %d", fileNumber, article.FileNumber)
	}
	if article.OriginalName != originalName {
		t.Errorf("Expected OriginalName to be %s, got %s", originalName, article.OriginalName)
	}
	if article.CustomHeaders["X-Test"] != customHeaders["X-Test"] {
		t.Errorf("Expected CustomHeaders[X-Test] to be %s, got %s", customHeaders["X-Test"], article.CustomHeaders["X-Test"])
	}
}

func TestEncode(t *testing.T) {
	// Create a test article
	article := &Article{
		MessageID:     "test-message-id",
		Subject:       "Test Subject",
		From:          "test@example.com",
		Groups:        []string{"alt.test", "alt.binaries.test"},
		PartNumber:    1,
		TotalParts:    3,
		FileSize:      int64(1000),
		FileName:      "test.txt",
		Offset:        0,
		Size:          100,
		Date:          time.Date(2023, 1, 1, 0, 0, 0, 0, time.UTC),
		CustomHeaders: map[string]string{"X-Test": "Value"},
	}

	// Test data to encode
	testData := []byte("This is test data for encoding with yEnc")

	// Update the Size field to match the actual test data length
	article.Size = uint64(len(testData))

	// Test Encode method
	reader, cleanup, err := article.Encode(testData)
	if err != nil {
		t.Fatalf("Encode failed: %v", err)
	}
	defer cleanup()

	// Read the encoded result
	encoded, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("Failed to read encoded data: %v", err)
	}

	result := string(encoded)

	// Verify it contains expected headers
	if !strings.Contains(result, "Subject: Test Subject") {
		t.Error("Encoded output missing Subject header")
	}
	if !strings.Contains(result, "From: test@example.com") {
		t.Error("Encoded output missing From header")
	}
	if !strings.Contains(result, "Newsgroups: alt.test,alt.binaries.test") {
		t.Error("Encoded output missing Newsgroups header")
	}
	if !strings.Contains(result, "Message-ID: <test-message-id>") {
		t.Error("Encoded output missing Message-ID header")
	}
	if !strings.Contains(result, "X-Test: Value") {
		t.Error("Encoded output missing custom header")
	}
	if !strings.Contains(result, "Date: Sun, 01 Jan 2023 00:00:00 +0000") {
		t.Error("Encoded output missing Date header")
	}

	// Verify yEnc encoding markers
	if !strings.Contains(result, "=ybegin") {
		t.Error("Encoded output missing yEnc begin marker")
	}
	if !strings.Contains(result, "=ypart") {
		t.Error("Encoded output missing yEnc part marker")
	}
	if !strings.Contains(result, "=yend") {
		t.Error("Encoded output missing yEnc end marker")
	}

	// Verify filename in yEnc headers
	if !strings.Contains(result, "name=test.txt") {
		t.Error("Encoded output missing filename in yEnc header")
	}

	// Verify the encoded data contains some expected yEnc patterns
	// yEnc typically uses characters in range 42-255 (excluding certain reserved chars)
	if len(result) == 0 {
		t.Error("Encoded output is empty")
	}
}

func TestEncodeWithXNxgHeader(t *testing.T) {
	// Create a test article with X-Nxg header
	article := &Article{
		MessageID:  "test-message-id",
		Subject:    "Test Subject",
		From:       "test@example.com",
		Groups:     []string{"alt.test"},
		PartNumber: 1,
		TotalParts: 1,
		FileSize:   int64(100),
		FileName:   "test.txt",
		Offset:     0,
		Size:       50,
		Date:       time.Date(2023, 1, 1, 0, 0, 0, 0, time.UTC),
		XNxgHeader: "test-nxg-header-value",
	}

	testData := []byte("Test data")

	// Update the Size field to match the actual test data length
	article.Size = uint64(len(testData))

	reader, cleanup, err := article.Encode(testData)
	if err != nil {
		t.Fatalf("Encode failed: %v", err)
	}
	defer cleanup()

	encoded, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("Failed to read encoded data: %v", err)
	}

	result := string(encoded)

	// Verify X-Nxg header is included
	if !strings.Contains(result, "X-Nxg: test-nxg-header-value") {
		t.Error("Encoded output missing X-Nxg header")
	}
}

func TestEncodeSmallData(t *testing.T) {
	// Create a test article with minimal data
	article := &Article{
		MessageID:  "test-message-id",
		Subject:    "Test Subject",
		From:       "test@example.com",
		Groups:     []string{"alt.test"},
		PartNumber: 1,
		TotalParts: 1,
		FileSize:   int64(1),
		FileName:   "small.txt",
		Offset:     0,
		Size:       1,
		Date:       time.Date(2023, 1, 1, 0, 0, 0, 0, time.UTC),
	}

	// Test with minimal data (1 byte)
	testData := []byte("x")

	reader, cleanup, err := article.Encode(testData)
	if err != nil {
		t.Fatalf("Encode with small data failed: %v", err)
	}
	defer cleanup()

	encoded, err := io.ReadAll(reader)
	if err != nil {
		t.Fatalf("Failed to read encoded small data: %v", err)
	}

	result := string(encoded)

	// Should still contain headers and yEnc markers even with minimal data
	if !strings.Contains(result, "Subject: Test Subject") {
		t.Error("Encoded small data missing Subject header")
	}
	if !strings.Contains(result, "=ybegin") {
		t.Error("Encoded small data missing yEnc begin marker")
	}
}

func TestGenerateMessageID(t *testing.T) {
	messageID := GenerateMessageID()

	// Check format: {rand(32)}@{rand(8)}.{rand(3)}
	pattern := `^[a-zA-Z0-9]{32}@[a-zA-Z0-9]{8}\.[a-zA-Z0-9]{3}$`
	match, err := regexp.MatchString(pattern, messageID)
	if err != nil {
		t.Fatalf("Regex match failed: %v", err)
	}
	if !match {
		t.Errorf("Generated message ID %s doesn't match expected pattern %s", messageID, pattern)
	}
}

func TestGenerateFrom(t *testing.T) {
	from := GenerateFrom()

	// Check format: {rand(14)} <{rand(14)}@{rand(5)}.{rand(3)}>
	pattern := `^[a-zA-Z0-9]{14} <[a-zA-Z0-9]{14}@[a-zA-Z0-9]{5}\.[a-zA-Z0-9]{3}>$`
	match, err := regexp.MatchString(pattern, from)
	if err != nil {
		t.Fatalf("Regex match failed: %v", err)
	}
	if !match {
		t.Errorf("Generated From %s doesn't match expected pattern %s", from, pattern)
	}
}

func TestGenerateSubject(t *testing.T) {
	fileNumber := 1
	totalFiles := 5
	fileName := "test.txt"
	partNumber := 2
	numSegments := 10

	subject := GenerateSubject(fileNumber, totalFiles, fileName, partNumber, numSegments)
	expected := `[1/5] "test.txt" - yEnc (2/10)`

	if subject != expected {
		t.Errorf("Expected subject to be %s, got %s", expected, subject)
	}
}

func TestGenerateRandomSubject(t *testing.T) {
	subject := GenerateRandomSubject()

	// Length should be 32
	if len(subject) != 32 {
		t.Errorf("Expected random subject length to be 32, got %d", len(subject))
	}

	// Should only contain alphanumeric characters
	pattern := `^[a-zA-Z0-9]+$`
	match, err := regexp.MatchString(pattern, subject)
	if err != nil {
		t.Fatalf("Regex match failed: %v", err)
	}
	if !match {
		t.Errorf("Generated random subject %s contains non-alphanumeric characters", subject)
	}
}

func TestRandomDateWithinLast6Hours(t *testing.T) {
	now := time.Now()
	sixHoursAgo := now.Add(-6 * time.Hour)

	randomDate := RandomDateWithinLast6Hours()

	if randomDate.Before(sixHoursAgo) || randomDate.After(now) {
		t.Errorf("Random date %v is not within last 6 hours (now: %v, 6h ago: %v)",
			randomDate, now, sixHoursAgo)
	}
}

func TestGenerateRandomFilename(t *testing.T) {
	filename := GenerateRandomFilename()

	// Length should be 32
	if len(filename) != 32 {
		t.Errorf("Expected random filename length to be 32, got %d", len(filename))
	}

	// Should only contain alphanumeric characters
	pattern := `^[a-zA-Z0-9]+$`
	match, err := regexp.MatchString(pattern, filename)
	if err != nil {
		t.Fatalf("Regex match failed: %v", err)
	}
	if !match {
		t.Errorf("Generated random filename %s contains non-alphanumeric characters", filename)
	}
}

func TestGenerateRandomString(t *testing.T) {
	// We need to test this internal function
	// Use reflection to access it if it's unexported

	// Testing three different lengths
	testLengths := []int{5, 10, 32}

	rexp := regexp.MustCompile(`^[a-zA-Z0-9]+$`)

	for _, length := range testLengths {
		// We have to call generateRandomString directly
		// This is a bit of a hack since we're testing an unexported function
		result := generateRandomString(length)

		if len(result) != length {
			t.Errorf("Expected random string length to be %d, got %d", length, len(result))
		}

		// Should only contain alphanumeric characters
		if !rexp.MatchString(result) {
			t.Errorf("Generated random string %s contains non-alphanumeric characters", result)
		}
	}
}
