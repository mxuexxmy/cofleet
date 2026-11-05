package main

import (
	"context"
	"encoding/json"
	"fmt"
	"gitee.com/linakesi/lzc-sdk/lang/go/sys"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
	"io"
	"os"
	"time"
)

type Input struct {
	Owner  string `json:"owner"`
	Action string `json:"action"`
}

func main() {
	if e := run(); e != nil {
		fmt.Fprintln(os.Stderr, e)
		os.Exit(1)
	}
}
func run() error {
	var in Input
	if e := json.NewDecoder(io.LimitReader(os.Stdin, 4096)).Decode(&in); e != nil {
		return e
	}
	if in.Owner == "" {
		return fmt.Errorf("authenticated owner required")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	ctx = metadata.AppendToOutgoingContext(ctx, "x-hc-user-id", in.Owner, "x_lzcapi_uid", in.Owner)
	addr := os.Getenv("LZCAPP_API_GATEWAY_ADDRESS")
	if addr == "" {
		addr = "unix:///lzcapp/run/sys/lzc-apis.socket"
	}
	c, e := grpc.DialContext(ctx, addr, grpc.WithTransportCredentials(insecure.NewCredentials()), grpc.WithBlock())
	if e != nil {
		return e
	}
	defer c.Close()
	client := sys.NewPackageManagerClient(c)
	v, e := client.QueryApplication(ctx, &sys.QueryApplicationRequest{})
	if e != nil {
		return e
	}
	var app *sys.AppInfo
	free := true
	for _, a := range v.GetInfoList() {
		if a.GetAppid() == "xyz.mxue.lzc.app.cofleet" {
			app = a
		}
		if a.GetSubdomain() == "test" && a.GetAppid() != "xyz.mxue.lzc.app.cofleet" {
			free = false
		}
	}
	if app == nil || app.GetOwner() != in.Owner {
		return fmt.Errorf("owned CoFleet deployment not found")
	}
	if in.Action == "query" {
		return json.NewEncoder(os.Stdout).Encode(map[string]any{"package": app.GetAppid(), "subdomain": app.GetSubdomain(), "domain": app.GetDomain(), "test_available": free})
	}
	if in.Action != "rename" && in.Action != "rollback" {
		return fmt.Errorf("unknown action")
	}
	next, expected := "test", "cofleet-test"
	if in.Action == "rollback" {
		next, expected = "cofleet-test", "test"
	}
	if app.GetSubdomain() == next {
		return json.NewEncoder(os.Stdout).Encode(map[string]any{"already_configured": next})
	}
	if app.GetSubdomain() != expected {
		return fmt.Errorf("unexpected current domain; refusing migration")
	}
	if in.Action == "rename" && !free {
		return fmt.Errorf("test domain occupied; refusing migration")
	}
	r, e := client.ChangeDeployCfg(ctx, &sys.ChangeDeployCfgRequest{DeployId: app.GetDeployId(), Subdomain: &next})
	if e != nil {
		return e
	}
	if r.GetResult() != sys.ChangeDeployCfgResponse_OK {
		return fmt.Errorf("domain change rejected: %s", r.GetResult())
	}
	return json.NewEncoder(os.Stdout).Encode(map[string]any{"subdomain": next, "changed": true})
}
