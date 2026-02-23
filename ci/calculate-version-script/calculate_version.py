import os
import re
import argparse
import sys
import toml
import yaml

def get_current_version(file_path):
    """
    Reads the current version from a Cargo.toml or Chart.yaml file.
    """
    if not os.path.exists(file_path):
        sys.stderr.write(f"Error: File '{file_path}' not found.\n")
        sys.exit(1)

    if file_path.endswith('Cargo.toml'):
        try:
            data = toml.load(file_path)
            # Support workspace.package.version (standard workspace) or package.version
            if 'workspace' in data and 'package' in data['workspace']:
                return data['workspace']['package']['version']
            elif 'package' in data:
                return data['package']['version']
            else:
                raise KeyError("Could not find [package] or [workspace.package] version")
        except Exception as e:
            sys.stderr.write(f"Error reading Cargo.toml: {e}\n")
            sys.exit(1)

    elif file_path.endswith('Chart.yaml'):
        try:
            with open(file_path, 'r') as f:
                data = yaml.safe_load(f)
            return data['version']
        except Exception as e:
            sys.stderr.write(f"Error reading Chart.yaml: {e}\n")
            sys.exit(1)
            
    else:
        sys.stderr.write(f"Error: Unsupported file type '{file_path}'. Use Cargo.toml or Chart.yaml.\n")
        sys.exit(1)

def calculate_next_version(current_version, changelog_dir):
    """
    Calculates the next semantic version based on changelog entries.
    """
    try:
        clean_version = current_version.lower().lstrip('v')
        major, minor, patch = map(int, clean_version.strip().split('.'))
    except ValueError:
        sys.stderr.write(f"Error: Invalid version format '{current_version}'. Expected X.Y.Z\n")
        sys.exit(1)
    
    entry_types = set()
    # Matches '123.added.md' AND '+orphan.added.md'
    pattern = re.compile(r'\.(added|changed|deprecated|removed|fixed|security|internal)\.md$')
    
    if not os.path.isdir(changelog_dir):
        sys.stderr.write(f"Error: Changelog directory '{changelog_dir}' does not exist.\n")
        sys.exit(1)

    files = os.listdir(changelog_dir)
    found_entries = False

    for filename in files:
        if filename.endswith('.md') and not filename.startswith('changelog_template'):
            match = pattern.search(filename)
            if match:
                entry_types.add(match.group(1))
                found_entries = True
    
    if not found_entries:
        return f"{major}.{minor}.{patch}"
    
    minor_triggers = {'added', 'changed', 'deprecated', 'removed', 'security'}
    
    is_minor = False
    for t in entry_types:
        if t in minor_triggers:
            is_minor = True
            break
            
    if is_minor:
        minor += 1
        patch = 0
    else:
        patch += 1
        
    return f"{major}.{minor}.{patch}"

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description='Calculate next version.')
    parser.add_argument('--file', required=True, help='Path to Cargo.toml or Chart.yaml')
    parser.add_argument('--changelog-dir', required=True, help='Path to changelog directory')
    
    args = parser.parse_args()
    
    current_version = get_current_version(args.file)
    print(calculate_next_version(current_version, args.changelog_dir))
