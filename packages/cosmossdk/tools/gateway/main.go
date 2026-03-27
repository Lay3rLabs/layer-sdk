package main

import (
	"context"
	"flag"
	"net/http"
	"strings"

	"github.com/golang/glog"
	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"github.com/rs/cors"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/grpclog"
	"google.golang.org/protobuf/encoding/protojson"

	auth "github.com/lay3rlabs/layer-sdk/gateway/cosmos/auth/v1beta1"
	bank "github.com/lay3rlabs/layer-sdk/gateway/cosmos/bank/v1beta1"
	tendermint "github.com/lay3rlabs/layer-sdk/gateway/cosmos/base/tendermint/v1beta1"
	tx "github.com/lay3rlabs/layer-sdk/gateway/cosmos/tx/v1beta1"
	cosmwasm "github.com/lay3rlabs/layer-sdk/gateway/cosmwasm/wasm/v1"
	sync "github.com/lay3rlabs/layer-sdk/gateway/layer/sync/v1"

	// to register pubkey any types
	_ "github.com/lay3rlabs/layer-sdk/gateway/cosmos/crypto/ed25519"
	_ "github.com/lay3rlabs/layer-sdk/gateway/cosmos/crypto/secp256k1"
)

var (
	// command-line options:
	// gRPC server endpoint
	grpcServerEndpoint = flag.String("grpc-server-endpoint", "localhost:9090", "gRPC server endpoint")
)

// Logic taken from the cosmos SDK code (server/api/server.go), so I guess it is needed
// We need to explicitly pass this header through (why? which client?)
const GRPCBlockHeightHeader = "x-cosmos-block-height"

func CustomGRPCHeaderMatcher(key string) (string, bool) {
	switch strings.ToLower(key) {
	case GRPCBlockHeightHeader:
		return GRPCBlockHeightHeader, true
	default:
		return runtime.DefaultHeaderMatcher(key)
	}
}

func run() error {
	ctx := context.Background()
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()

	// Create the grpc proxy mux, with custom header support
	mux := runtime.NewServeMux(
		runtime.WithIncomingHeaderMatcher(CustomGRPCHeaderMatcher),
		// This will use lower-case names (what we find in .proto), not the Golang camelCase names
		runtime.WithMarshalerOption(runtime.MIMEWildcard, &runtime.JSONPb{
			MarshalOptions: protojson.MarshalOptions{
				UseProtoNames:   true,
				EmitUnpopulated: true,
			},
			UnmarshalOptions: protojson.UnmarshalOptions{
				DiscardUnknown: true,
			},
		}),
	)

	endpoint := *grpcServerEndpoint
	opts := []grpc.DialOption{grpc.WithTransportCredentials(insecure.NewCredentials())}

	// create a connection to the underlying grpc server
	// Note: Make sure the gRPC server is running properly and accessible
	conn, err := grpc.DialContext(ctx, endpoint, opts...)
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			if cerr := conn.Close(); cerr != nil {
				grpclog.Infof("Failed to close conn to %s: %v", endpoint, cerr)
			}
			return
		}
		go func() {
			<-ctx.Done()
			if cerr := conn.Close(); cerr != nil {
				grpclog.Infof("Failed to close conn to %s: %v", endpoint, cerr)
			}
		}()
	}()

	// Register the gateway handlers
	err = auth.RegisterQueryHandler(ctx, mux, conn)
	if err != nil {
		return err
	}
	err = bank.RegisterQueryHandler(ctx, mux, conn)
	if err != nil {
		return err
	}
	err = cosmwasm.RegisterQueryHandler(ctx, mux, conn)
	if err != nil {
		return err
	}
	err = sync.RegisterQueryHandler(ctx, mux, conn)
	if err != nil {
		return err
	}
	err = tendermint.RegisterServiceHandler(ctx, mux, conn)
	if err != nil {
		return err
	}
	err = tx.RegisterServiceHandler(ctx, mux, conn)
	if err != nil {
		return err
	}

	// TODO: config via env vars or such
	c := cors.New(cors.Options{
		// AllowedOrigins:   []string{"https://*.cav3.net", "https://*.dev-cav3.net", "http://localhost:*"},
		AllowedOrigins:   []string{"*"},
		AllowCredentials: true,
		// Enable Debugging for testing, consider disabling in production
		Debug: true,
	})
	handler := c.Handler(mux)

	// Start HTTP server (and proxy calls to gRPC server endpoint)
	return http.ListenAndServe("0.0.0.0:1317", handler)
}

func main() {
	flag.Parse()
	defer glog.Flush()

	if err := run(); err != nil {
		glog.Fatal(err)
	}
}
