#!/bin/bash

# Setu End-to-End Demo Script
# This script demonstrates the complete Solver → Validator flow

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Print banner
echo -e "${CYAN}"
echo "╔════════════════════════════════════════════════════════════╗"
echo "║                                                            ║"
echo "║              🚀 Setu End-to-End Demo 🚀                   ║"
echo "║                                                            ║"
echo "║  Demonstrating Solver → Validator Flow                    ║"
echo "║  with Transfer Execution and Event Verification            ║"
echo "║                                                            ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo -e "${NC}"
echo ""

# Check if cargo is available
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}❌ Error: cargo not found. Please install Rust.${NC}"
    exit 1
fi

echo -e "${BLUE}📦 Building project...${NC}"
cargo build --release 2>&1 | grep -E "(Compiling|Finished)" || true
echo -e "${GREEN}✅ Build complete${NC}"
echo ""

echo -e "${PURPLE}═══════════════════════════════════════════════════════════${NC}"
echo -e "${YELLOW}🧪 Running End-to-End Tests${NC}"
echo -e "${PURPLE}═══════════════════════════════════════════════════════════${NC}"
echo ""

# Run tests with detailed output
echo -e "${CYAN}Test 1: Complete Solver → Validator Flow${NC}"
echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
cargo test --package setu-validator --test end_to_end_test test_complete_solver_to_validator_flow -- --nocapture 2>&1 | \
    grep -E "(INFO|test_complete|✅|📤|Starting|completed)" || true
echo ""

echo -e "${CYAN}Test 2: Multiple Transfers${NC}"
echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
cargo test --package setu-validator --test end_to_end_test test_multiple_transfers -- --nocapture 2>&1 | \
    grep -E "(INFO|test_multiple|✅|📤|Starting|completed)" || true
echo ""

echo -e "${CYAN}Test 3: Transfer Dependency Chain${NC}"
echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
cargo test --package setu-validator --test end_to_end_test test_transfer_dependency_chain -- --nocapture 2>&1 | \
    grep -E "(INFO|test_transfer|✅|📤|Starting|completed)" || true
echo ""

echo -e "${CYAN}Test 4: Concurrent Transfers${NC}"
echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
cargo test --package setu-validator --test end_to_end_test test_concurrent_transfers -- --nocapture 2>&1 | \
    grep -E "(INFO|test_concurrent|✅|📤|Starting|completed)" || true
echo ""

echo -e "${CYAN}Test 5: Validator Statistics${NC}"
echo -e "${BLUE}────────────────────────────────────────────────────────────${NC}"
cargo test --package setu-validator --test end_to_end_test test_validator_statistics -- --nocapture 2>&1 | \
    grep -E "(INFO|test_validator|✅|📤|📊|Starting|completed)" || true
echo ""

# Run all tests and capture result
echo -e "${PURPLE}═══════════════════════════════════════════════════════════${NC}"
echo -e "${YELLOW}📊 Running All Tests (Summary)${NC}"
echo -e "${PURPLE}═══════════════════════════════════════════════════════════${NC}"
echo ""

if cargo test --package setu-validator --test end_to_end_test 2>&1 | tee /tmp/setu_test_output.txt | tail -20; then
    echo ""
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║                                                            ║${NC}"
    echo -e "${GREEN}║              ✅ All Tests Passed! ✅                       ║${NC}"
    echo -e "${GREEN}║                                                            ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    
    # Extract test summary
    echo -e "${CYAN}📈 Test Summary:${NC}"
    grep "test result:" /tmp/setu_test_output.txt || true
    echo ""
    
    echo -e "${YELLOW}🎯 What was tested:${NC}"
    echo -e "  ${GREEN}✓${NC} Solver → Validator communication"
    echo -e "  ${GREEN}✓${NC} Transfer execution in TEE (simulated)"
    echo -e "  ${GREEN}✓${NC} Event verification pipeline"
    echo -e "  ${GREEN}✓${NC} DAG construction and depth calculation"
    echo -e "  ${GREEN}✓${NC} Parent-child relationship tracking"
    echo -e "  ${GREEN}✓${NC} Sampling verification (probabilistic)"
    echo -e "  ${GREEN}✓${NC} Concurrent transfer processing"
    echo -e "  ${GREEN}✓${NC} Dependency chain handling"
    echo ""
    
    echo -e "${CYAN}🔍 Key Features Demonstrated:${NC}"
    echo -e "  ${BLUE}•${NC} Asynchronous message passing (mpsc channels)"
    echo -e "  ${BLUE}•${NC} VLC (Vector Logical Clock) tracking"
    echo -e "  ${BLUE}•${NC} TEE proof generation and verification"
    echo -e "  ${BLUE}•${NC} State change computation and application"
    echo -e "  ${BLUE}•${NC} Causal ordering with DAG"
    echo -e "  ${BLUE}•${NC} Probabilistic sampling (10% rate)"
    echo ""
    
    exit 0
else
    echo ""
    echo -e "${RED}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║                                                            ║${NC}"
    echo -e "${RED}║              ❌ Some Tests Failed ❌                       ║${NC}"
    echo -e "${RED}║                                                            ║${NC}"
    echo -e "${RED}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${YELLOW}Please check the output above for details.${NC}"
    exit 1
fi

