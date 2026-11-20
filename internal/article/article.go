package article

import (
	"bytes"
	"crypto/rand"
	"fmt"
	"io"
	"maps"
	"math/big"
	mrand "math/rand"
	"strings"
	"sync"
	"time"

	"github.com/mnightingale/rapidyenc"
)

// encoderBufferPool provides reusable buffers for article encoding to reduce GC pressure
var encoderBufferPool = sync.Pool{
	New: func() any {
		// Pre-allocate 900KB (slightly larger than typical article size)
		return bytes.NewBuffer(make([]byte, 0, 900*1024))
	},
}

type Encoder interface {
	Encode(p []byte) []byte
}

// Article represents a Usenet article
type Article struct {
	MessageID       string
	Subject         string
	OriginalSubject string
	From            string
	Groups          []string
	PartNumber      int
	TotalParts      int
	FileName        string
	Date            time.Time
	FileNumber      int
	Offset          int64
	Size            uint64
	FileSize        int64
	OriginalName    string
	CustomHeaders   map[string]string
	XNxgHeader      string
	Hash            string
}

// New creates a new Article
func New(
	messageID,
	subject,
	originalSubject,
	from string,
	groups []string,
	partNumber,
	totalParts int,
	fileSize int64,
	fileName string,
	fileNumber int,
	originalName string,
	customHeaders map[string]string,
) *Article {
	return &Article{
		MessageID:       messageID,
		Subject:         subject,
		OriginalSubject: originalSubject,
		From:            from,
		Groups:          groups,
		PartNumber:      partNumber,
		TotalParts:      totalParts,
		FileSize:        fileSize,
		FileName:        fileName,
		FileNumber:      fileNumber,
		OriginalName:    originalName,
		Date:            time.Now(),
		CustomHeaders:   customHeaders,
	}
}

// Encode encodes the article body using yEnc encoding.
// Returns the encoded reader, a cleanup function to return the buffer to the pool, and any error.
// The caller MUST call the cleanup function when done with the reader.
func (a *Article) Encode(body []byte) (io.Reader, func(), error) {
	headers := make(map[string]string)

	if a.CustomHeaders != nil {
		maps.Copy(headers, a.CustomHeaders)
	}

	headers["Subject"] = a.Subject
	headers["From"] = a.From
	headers["Newsgroups"] = strings.Join(a.Groups, ",")
	headers["Message-ID"] = fmt.Sprintf("<%s>", a.MessageID)
	headers["Date"] = a.Date.UTC().Format(time.RFC1123Z)

	if a.XNxgHeader != "" {
		headers["X-Nxg"] = a.XNxgHeader
	}

	var header strings.Builder
	for k, v := range headers {
		header.WriteString(fmt.Sprintf("%s: %s\r\n", k, v))
	}

	// Get buffer from pool and reset it
	buff := encoderBufferPool.Get().(*bytes.Buffer)
	buff.Reset()

	// Cleanup function to return buffer to pool
	cleanup := func() {
		encoderBufferPool.Put(buff)
	}

	meta := rapidyenc.Meta{
		FileName:   a.FileName,
		FileSize:   a.FileSize,
		PartNumber: int64(a.PartNumber),
		TotalParts: int64(a.TotalParts),
		Offset:     int64(a.Offset),
		PartSize:   int64(a.Size),
	}
	if err := encodeTo(buff, header.String(), meta, body); err != nil {
		cleanup() // Return buffer to pool on error
		return nil, nil, err
	}

	return buff, cleanup, nil
}

// encodeTo writes the header lines, the blank separator line and the yEnc-encoded part to buff.
func encodeTo(buff *bytes.Buffer, header string, meta rapidyenc.Meta, body []byte) error {
	buff.WriteString(header + "\r\n")

	encoder, err := rapidyenc.NewEncoder(buff, meta)
	if err != nil {
		return fmt.Errorf("error creating encoder: %w", err)
	}

	_, errWrite := encoder.Write(body)

	if err := encoder.Close(); err != nil {
		return fmt.Errorf("error closing encoder: %w", err)
	}

	if errWrite != nil {
		return fmt.Errorf("error writing article body: %w", errWrite)
	}

	return nil
}

// EncodeWire returns the bytes of one article as posted: the already formatted header
// lines (each CRLF-terminated), a blank line, then the yEnc part (=ybegin, =ypart, data,
// =yend with pcrc32). It is the pure core of [Article.Encode].
func EncodeWire(header, fileName string, fileSize, partNumber, totalParts, offset, partSize int64, body []byte) ([]byte, error) {
	var buff bytes.Buffer
	meta := rapidyenc.Meta{
		FileName:   fileName,
		FileSize:   fileSize,
		PartNumber: partNumber,
		TotalParts: totalParts,
		Offset:     offset,
		PartSize:   partSize,
	}
	if err := encodeTo(&buff, header, meta, body); err != nil {
		return nil, err
	}
	return buff.Bytes(), nil
}

// generateRandomString generates a random string of specified length.
// crypto/rand never returns an error (Go 1.24+: it crashes the program instead),
// so there is no error path to propagate.
func generateRandomString(length int) string {
	const charset = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	max := big.NewInt(int64(len(charset)))
	result := make([]byte, length)
	for i := range result {
		idx, _ := rand.Int(rand.Reader, max)
		result[i] = charset[idx.Int64()]
	}
	return string(result)
}

// GenerateMessageID generates a message ID following the obfuscation pattern.
func GenerateMessageID() string {
	// Format: {rand(32)}@{rand(8)}.{rand(3)}
	return fmt.Sprintf("%s@%s.%s", generateRandomString(32), generateRandomString(8), generateRandomString(3))
}

// GenerateFrom generates a From header following the obfuscation pattern.
func GenerateFrom() string {
	// Format: {rand(14)} <{rand(14)}@{rand(5)}.{rand(3)}>
	return fmt.Sprintf("%s <%s@%s.%s>", generateRandomString(14), generateRandomString(14), generateRandomString(5), generateRandomString(3))
}

// GenerateSubject generates a subject following the obfuscation pattern
func GenerateSubject(fileNumber int, totalFiles int, fileName string, partNumber int, numSegments int) string {
	return fmt.Sprintf("[%v/%v] \"%v\" - yEnc (%v/%v)", fileNumber, totalFiles, fileName, partNumber, numSegments)
}

func GenerateRandomSubject() string {
	return generateRandomString(32)
}

func RandomDateWithinLast6Hours() time.Time {
	now := time.Now()
	millisecondsIn6Hours := 6 * 60 * 60 * 1000
	randomMilliseconds := mrand.Intn(millisecondsIn6Hours)
	randomDuration := time.Duration(randomMilliseconds) * time.Millisecond
	randomDate := now.Add(-randomDuration)

	return randomDate
}

// GenerateRandomFilename generates a random filename
func GenerateRandomFilename() string {
	return generateRandomString(32)
}
