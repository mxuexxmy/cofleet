package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	sdk "gitee.com/linakesi/lzc-sdk/lang/go"
	"gitee.com/linakesi/lzc-sdk/lang/go/common"
	"gitee.com/linakesi/lzc-sdk/lang/go/localdevice"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/emptypb"
	"io"
	"net/http"
	"net/url"
	"os"
	"regexp"
	"strings"
	"time"
)

type Input struct {
	Owner            string
	Peer             string
	Service          string
	Secret           string
	Nonce            string
	ExpectedDeviceID string
	Case             string
}

func run() error {
	var in Input
	if err := json.NewDecoder(io.LimitReader(os.Stdin, 8192)).Decode(&in); err != nil {
		return errors.New("invalid input")
	}
	if in.Owner == "" || in.Peer == "" || !regexp.MustCompile("^xyz[.]mxue[.]lzc[.]app[.]cofleet[.]stage0[.][a-f0-9]{32}$").MatchString(in.Service) || len(in.Secret) < 32 {
		return errors.New("invalid scope")
	}
	gateway := os.Getenv("LZCAPP_API_GATEWAY_ADDRESS")
	if gateway == "" {
		return errors.New("platform gateway environment missing")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	conn, err := grpc.DialContext(ctx, gateway, grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if err != nil {
		return errors.New("internal platform RPC unavailable")
	}
	defer conn.Close()
	identity := metadata.NewOutgoingContext(ctx, metadata.Pairs("x-hc-user-id", in.Owner, "x_lzcapi_uid", in.Owner))
	list, err := common.NewEndDeviceServiceClient(conn).ListEndDevices(identity, &common.ListEndDeviceRequest{Uid: in.Owner})
	if err != nil {
		if in.Case == "foreign-owner" && (status.Code(err) == codes.PermissionDenied || status.Code(err) == codes.Unauthenticated || status.Code(err) == codes.NotFound) {
			return json.NewEncoder(os.Stdout).Encode(map[string]any{"ownership_rejected": true})
		}
		return errors.New("ownership query failed")
	}
	var owned *common.EndDevice
	for _, d := range list.GetDevices() {
		if d.GetUniqueDeivceId() == in.Peer || d.GetAvoidUseThisPeerId() == in.Peer {
			owned = d
			break
		}
	}
	if owned == nil || !owned.GetIsOnline() {
		if in.Case == "foreign-owner" {
			return json.NewEncoder(os.Stdout).Encode(map[string]any{"ownership_rejected": true})
		}
		return errors.New("device not online or not owned")
	}
	target, err := url.Parse(owned.GetDeviceApiUrl())
	if err != nil || target.Scheme != "https" || target.Host == "" || target.User != nil || target.RawQuery != "" {
		return errors.New("invalid verified device URL")
	}
	roots, err := x509.SystemCertPool()
	if err != nil {
		roots = x509.NewCertPool()
	}
	ca, err := os.ReadFile("/lzcapp/run/certs/box.crt")
	if err != nil || !roots.AppendCertsFromPEM(ca) {
		return errors.New("box trust certificate unavailable")
	}
	cert, err := tls.LoadX509KeyPair("/lzcapp/run/certs/app.crt", "/lzcapp/run/certs/app.key")
	if err != nil {
		return errors.New("application identity unavailable")
	}
	config := &tls.Config{RootCAs: roots, Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS12}
	remote, err := grpc.DialContext(identity, target.Host, grpc.WithTransportCredentials(credentials.NewTLS(config)), grpc.WithBlock())
	if err != nil {
		return errors.New("verified device TLS connection failed")
	}
	defer remote.Close()
	token, err := sdk.RequestAuthToken(identity, remote)
	if err != nil {
		return errors.New("device authorization failed")
	}
	if in.Case == "device-info" {
		info, queryErr := localdevice.NewDeviceServiceClient(remote).Query(metadata.AppendToOutgoingContext(identity, "lzc_dapi_auth_token", token.Token), &emptypb.Empty{})
		if queryErr != nil {
			return errors.New("device information query failed")
		}
		return json.NewEncoder(os.Stdout).Encode(map[string]any{"os": info.GetOS(), "cpu": info.GetCPU(), "name": info.GetName(), "ownership_verified": true, "verified_tls": true})
	}
	target.Path = strings.TrimRight(target.Path, "/") + "/s/" + in.Service + "/status"
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, target.String(), nil)
	if err != nil {
		return errors.New("callback construction failed")
	}
	switch in.Case {
	case "anonymous":
	case "wrong-platform-token":
		request.Header.Set("lzc_dapi_auth_token", "invalid-stage0-device-token")
	case "wrong-probe-secret":
		request.Header.Set("X-CoFleet-Probe-Key", "invalid-stage0-probe-secret")
	case "probe-secret-only":
		request.Header.Set("X-CoFleet-Probe-Key", in.Secret)
	case "platform-token-only":
		request.Header.Set("lzc_dapi_auth_token", token.Token)
	default:
		request.Header.Set("lzc_dapi_auth_token", token.Token)
		request.Header.Set("X-CoFleet-Probe-Key", in.Secret)
	}
	httpTLS := config.Clone()
	httpTLS.Certificates = nil // HTTP authorization must use the device token, not an mTLS fallback.
	client := &http.Client{Timeout: 10 * time.Second, Transport: &http.Transport{TLSClientConfig: httpTLS}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	response, err := client.Do(request)
	if err != nil {
		return errors.New("verified HTTPS callback failed")
	}
	defer response.Body.Close()
	if in.Case == "anonymous" || in.Case == "wrong-platform-token" || in.Case == "wrong-probe-secret" {
		if response.StatusCode != 401 && response.StatusCode != 403 {
			return fmt.Errorf("unauthorized probe unexpectedly returned HTTP %d", response.StatusCode)
		}
		return json.NewEncoder(os.Stdout).Encode(map[string]any{"unauthorized_rejected": true, "status": response.StatusCode})
	}
	if (in.Case == "probe-secret-only" || in.Case == "platform-token-only") && (response.StatusCode == 401 || response.StatusCode == 403) {
		return json.NewEncoder(os.Stdout).Encode(map[string]any{"credential_rejected": true, "status": response.StatusCode})
	}
	if response.StatusCode != 200 {
		return fmt.Errorf("callback HTTP %d", response.StatusCode)
	}
	var reply struct {
		Nonce string
		Agent map[string]any
	}
	if json.NewDecoder(io.LimitReader(response.Body, 16384)).Decode(&reply) != nil || reply.Nonce != in.Nonce || reply.Agent["device_id"] != in.ExpectedDeviceID {
		return errors.New("invalid probe reply")
	}
	return json.NewEncoder(os.Stdout).Encode(map[string]any{"ownership_verified": true, "verified_tls": true, "device_token_obtained": true, "gateway_callback": true, "challenge_verified": true, "agent_status_read": true})
}
func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err.Error())
		os.Exit(1)
	}
}
